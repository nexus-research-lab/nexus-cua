//! Bounded `AXUIElement` snapshot and semantic-action actor.

use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::ptr;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread;
use std::time::{Duration, Instant};

use accessibility_sys::{
    AXUIElementCopyActionNames, AXUIElementCopyAttributeValue, AXUIElementCopyElementAtPosition,
    AXUIElementCopyMultipleAttributeValues, AXUIElementCreateApplication, AXUIElementGetTypeID,
    AXUIElementIsAttributeSettable, AXUIElementPerformAction, AXUIElementRef,
    AXUIElementSetAttributeValue, AXUIElementSetMessagingTimeout, AXValueCreate, AXValueGetType,
    AXValueGetValue, AXValueRef, kAXChildrenAttribute, kAXDescriptionAttribute,
    kAXEnabledAttribute, kAXErrorCannotComplete, kAXErrorSuccess, kAXExpandedAttribute,
    kAXFocusedAttribute, kAXFrontmostAttribute, kAXMainAttribute, kAXMaxValueAttribute,
    kAXMinValueAttribute, kAXMinimizedAttribute, kAXParentAttribute, kAXPickAction,
    kAXPositionAttribute, kAXPressAction, kAXRaiseAction, kAXRoleAttribute,
    kAXSecureTextFieldSubrole, kAXSelectedAttribute, kAXSizeAttribute, kAXSubroleAttribute,
    kAXTitleAttribute, kAXValueAttribute, kAXValueIncrementAttribute, kAXValueTypeCGPoint,
    kAXValueTypeCGSize, kAXWindowAttribute, kAXWindowsAttribute,
};
use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{CFGetTypeID, CFType, CFTypeRef, TCFType, TCFTypeRef};
use core_foundation::boolean::CFBoolean;
use core_foundation::number::CFNumber;
use core_foundation::string::CFString;
use core_foundation::{declare_TCFType, impl_CFTypeDescription, impl_TCFType};
use core_graphics::geometry::{CGPoint, CGSize};
use nexus_cua_protocol::{
    AccessibilityMode, AccessibilityRangeValue, ObservationTruncation, ScreenPoint, ScreenRect,
    SensitiveText, TruncationReason,
};
use nexus_cua_runtime::{DriverElement, DriverError, DriverErrorKind};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

const COMMAND_CAPACITY: usize = 32;
const SNAPSHOT_CACHE_LIMIT: usize = 32;
const AX_MESSAGING_TIMEOUT_SECONDS: f32 = 0.2;
const MAX_FIELD_BYTES: usize = 512;
const WINDOW_CONFIRM_TIMEOUT: Duration = Duration::from_millis(200);
const WINDOW_CONFIRM_INTERVAL: Duration = Duration::from_millis(10);

declare_TCFType!(AxElement, AXUIElementRef);
impl_TCFType!(AxElement, AXUIElementRef, AXUIElementGetTypeID);
impl_CFTypeDescription!(AxElement);

impl AxElement {
    fn application(pid: i32) -> Self {
        // SAFETY: The create-rule function returns a retained accessibility
        // application object for the supplied process identifier.
        unsafe { Self::wrap_under_create_rule(AXUIElementCreateApplication(pid)) }
    }

    fn set_messaging_timeout(&self, timeout_seconds: f32) -> Result<(), DriverError> {
        // SAFETY: The retained element is live for this synchronous call.
        let error =
            unsafe { AXUIElementSetMessagingTimeout(self.as_concrete_TypeRef(), timeout_seconds) };
        if error == kAXErrorSuccess {
            Ok(())
        } else {
            Err(provider_failure("failed to set AX messaging timeout"))
        }
    }
}

#[derive(Clone)]
pub(super) struct SemanticActor {
    sender: SyncSender<SemanticCommand>,
}

pub(super) struct SemanticSnapshot {
    pub(super) elements: Vec<DriverElement>,
    pub(super) complete: bool,
    pub(super) truncation: Option<ObservationTruncation>,
}

pub(super) enum SemanticAction {
    Focus,
    Invoke,
    SetValue(SensitiveText),
    SetRangeValue(f64),
    Toggle,
    Select,
    SetExpanded(bool),
}

impl SemanticActor {
    pub(super) fn spawn() -> Self {
        let (sender, receiver) = sync_channel(COMMAND_CAPACITY);
        thread::Builder::new()
            .name("nexus-cua-macos-semantic".to_owned())
            .spawn(move || SemanticState::default().run(&receiver))
            .expect("create AXUIElement actor thread");
        Self { sender }
    }

    pub(super) async fn snapshot(
        &self,
        pid: i32,
        window_bounds: ScreenRect,
        window_title: String,
        mode: AccessibilityMode,
    ) -> Result<SemanticSnapshot, DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.admit(SemanticCommand::Snapshot {
            pid,
            window_bounds,
            window_title,
            mode,
            reply,
        })?;
        receiver.await.map_err(actor_stopped)?
    }

    pub(super) async fn perform(
        &self,
        element_key: String,
        action: SemanticAction,
    ) -> Result<(), DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.admit(SemanticCommand::Perform {
            element_key,
            action,
            reply,
        })
        .map_err(DriverError::mutation_not_dispatched)?;
        receiver.await.map_err(actor_stopped)?
    }

    pub(super) async fn invoke_at_point(
        &self,
        pid: i32,
        window_bounds: ScreenRect,
        window_title: String,
        point: ScreenPoint,
    ) -> Result<bool, DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.admit(SemanticCommand::InvokeAtPoint {
            pid,
            window_bounds,
            window_title,
            point,
            reply,
        })
        .map_err(DriverError::mutation_not_dispatched)?;
        receiver.await.map_err(actor_stopped)?
    }

    pub(super) async fn raise_window(
        &self,
        pid: i32,
        window_bounds: ScreenRect,
        window_title: String,
    ) -> Result<(), DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.admit(SemanticCommand::RaiseWindow {
            pid,
            window_bounds,
            window_title,
            reply,
        })?;
        receiver.await.map_err(actor_stopped)?
    }

    pub(super) async fn set_window_bounds(
        &self,
        pid: i32,
        current_bounds: ScreenRect,
        window_title: String,
        requested_bounds: ScreenRect,
    ) -> Result<(), DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.admit(SemanticCommand::SetWindowBounds {
            pid,
            current_bounds,
            window_title,
            requested_bounds,
            reply,
        })
        .map_err(DriverError::mutation_not_dispatched)?;
        receiver.await.map_err(actor_stopped)?
    }

    pub(super) async fn set_window_minimized(
        &self,
        pid: i32,
        current_bounds: ScreenRect,
        window_title: String,
        minimized: bool,
    ) -> Result<(), DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.admit(SemanticCommand::SetWindowMinimized {
            pid,
            current_bounds,
            window_title,
            minimized,
            reply,
        })
        .map_err(DriverError::mutation_not_dispatched)?;
        receiver.await.map_err(actor_stopped)?
    }

    fn admit(&self, command: SemanticCommand) -> Result<(), DriverError> {
        self.sender.try_send(command).map_err(|error| match error {
            TrySendError::Full(_) => {
                DriverError::new(DriverErrorKind::Busy, "macOS semantic actor is busy")
                    .retryable("retry_with_backoff")
            }
            TrySendError::Disconnected(_) => actor_stopped(()),
        })
    }
}

enum SemanticCommand {
    Snapshot {
        pid: i32,
        window_bounds: ScreenRect,
        window_title: String,
        mode: AccessibilityMode,
        reply: oneshot::Sender<Result<SemanticSnapshot, DriverError>>,
    },
    Perform {
        element_key: String,
        action: SemanticAction,
        reply: oneshot::Sender<Result<(), DriverError>>,
    },
    InvokeAtPoint {
        pid: i32,
        window_bounds: ScreenRect,
        window_title: String,
        point: ScreenPoint,
        reply: oneshot::Sender<Result<bool, DriverError>>,
    },
    RaiseWindow {
        pid: i32,
        window_bounds: ScreenRect,
        window_title: String,
        reply: oneshot::Sender<Result<(), DriverError>>,
    },
    SetWindowBounds {
        pid: i32,
        current_bounds: ScreenRect,
        window_title: String,
        requested_bounds: ScreenRect,
        reply: oneshot::Sender<Result<(), DriverError>>,
    },
    SetWindowMinimized {
        pid: i32,
        current_bounds: ScreenRect,
        window_title: String,
        minimized: bool,
        reply: oneshot::Sender<Result<(), DriverError>>,
    },
}

#[derive(Default)]
struct SemanticState {
    next_snapshot: u64,
    snapshots: VecDeque<StoredSnapshot>,
}

struct StoredSnapshot {
    elements: HashMap<String, StoredElement>,
}

struct StoredElement {
    element: AxElement,
    signature: [u8; 32],
}

struct SnapshotBudget {
    max_nodes: usize,
    max_depth: usize,
    max_bytes: usize,
    deadline: Instant,
}

struct NodeValues {
    role: String,
    name: String,
    value: Option<String>,
    range_value: Option<AccessibilityRangeValue>,
    enabled: bool,
    focused: bool,
    selected: Option<bool>,
    checked: Option<bool>,
    expanded: Option<bool>,
    bounds: Option<ScreenRect>,
    children: Vec<AxElement>,
}

impl SemanticState {
    fn run(mut self, receiver: &Receiver<SemanticCommand>) {
        while let Ok(command) = receiver.recv() {
            match command {
                SemanticCommand::Snapshot {
                    pid,
                    window_bounds,
                    window_title,
                    mode,
                    reply,
                } => {
                    let _ = reply.send(self.snapshot(pid, window_bounds, &window_title, mode));
                }
                SemanticCommand::Perform {
                    element_key,
                    action,
                    reply,
                } => {
                    let _ = reply.send(self.perform(&element_key, action));
                }
                SemanticCommand::InvokeAtPoint {
                    pid,
                    window_bounds,
                    window_title,
                    point,
                    reply,
                } => {
                    let _ = reply.send(invoke_at_point(pid, window_bounds, &window_title, point));
                }
                SemanticCommand::RaiseWindow {
                    pid,
                    window_bounds,
                    window_title,
                    reply,
                } => {
                    let _ = reply.send(raise_window(pid, window_bounds, &window_title));
                }
                SemanticCommand::SetWindowBounds {
                    pid,
                    current_bounds,
                    window_title,
                    requested_bounds,
                    reply,
                } => {
                    let _ = reply.send(set_window_bounds(
                        pid,
                        current_bounds,
                        &window_title,
                        requested_bounds,
                    ));
                }
                SemanticCommand::SetWindowMinimized {
                    pid,
                    current_bounds,
                    window_title,
                    minimized,
                    reply,
                } => {
                    let _ = reply.send(set_window_minimized(
                        pid,
                        current_bounds,
                        &window_title,
                        minimized,
                    ));
                }
            }
        }
    }

    fn snapshot(
        &mut self,
        pid: i32,
        window_bounds: ScreenRect,
        window_title: &str,
        mode: AccessibilityMode,
    ) -> Result<SemanticSnapshot, DriverError> {
        ensure_accessibility_permission()?;
        let root = find_window(pid, window_bounds, window_title)?;
        self.next_snapshot = self.next_snapshot.wrapping_add(1);
        let snapshot_key = format!("ax:{pid}:{}", self.next_snapshot);
        let budget = snapshot_budget(mode);
        let mut queue = VecDeque::from([(root, None, 0_usize)]);
        let mut elements = Vec::new();
        let mut stored = HashMap::new();
        let mut visited = 0_usize;
        let mut aggregate_bytes = 0_usize;
        let mut truncation = None;

        while let Some((element, nearest_parent, depth)) = queue.pop_front() {
            if Instant::now() >= budget.deadline {
                truncation = Some(TruncationReason::Deadline);
                break;
            }
            if visited >= budget.max_nodes {
                truncation = Some(TruncationReason::NodeLimit);
                break;
            }
            if depth > budget.max_depth {
                truncation.get_or_insert(TruncationReason::DepthLimit);
                continue;
            }
            visited += 1;
            let values = match read_node(&element) {
                Ok(values) => values,
                Err(error) if error.kind == DriverErrorKind::TargetUnresponsive => {
                    return Err(error);
                }
                Err(error) if error.retryable => {
                    truncation = Some(TruncationReason::ProviderFailure);
                    break;
                }
                Err(_) => continue,
            };
            let actions = normalized_actions(&values.role, values.range_value);
            let emit = mode == AccessibilityMode::Full
                || !actions.is_empty()
                || !values.name.is_empty()
                || depth <= 1;
            let current_parent = if emit {
                let key = format!("{snapshot_key}:{}", elements.len());
                aggregate_bytes = aggregate_bytes
                    .saturating_add(values.role.len())
                    .saturating_add(values.name.len())
                    .saturating_add(values.value.as_ref().map_or(0, String::len));
                if aggregate_bytes > budget.max_bytes {
                    truncation = Some(TruncationReason::ByteLimit);
                    break;
                }
                let signature = element_signature(&values);
                elements.push(DriverElement {
                    key: key.clone(),
                    parent_key: nearest_parent.clone(),
                    role: normalize_role(&values.role),
                    name: values.name,
                    value: values.value,
                    range_value: values.range_value,
                    screen_bounds: values.bounds,
                    enabled: values.enabled,
                    focused: values.focused,
                    selected: values.selected,
                    checked: values.checked,
                    expanded: values.expanded,
                    actions,
                });
                stored.insert(key.clone(), StoredElement { element, signature });
                Some(key)
            } else {
                nearest_parent
            };
            for child in values.children {
                queue.push_back((child, current_parent.clone(), depth + 1));
            }
        }

        propagate_table_row_names(&mut elements);

        self.snapshots
            .push_back(StoredSnapshot { elements: stored });
        while self.snapshots.len() > SNAPSHOT_CACHE_LIMIT {
            self.snapshots.pop_front();
        }
        let truncation = truncation.map(|reason| ObservationTruncation {
            reason,
            emitted_elements: u32::try_from(elements.len()).unwrap_or(u32::MAX),
        });
        Ok(SemanticSnapshot {
            elements,
            complete: truncation.is_none(),
            truncation,
        })
    }

    fn perform(&self, element_key: &str, action: SemanticAction) -> Result<(), DriverError> {
        ensure_accessibility_permission()?;
        let stored = self
            .snapshots
            .iter()
            .rev()
            .find_map(|snapshot| snapshot.elements.get(element_key))
            .ok_or_else(|| stale_element().mutation_not_dispatched())?;
        let current = read_action_identity(&stored.element).map_err(|error| {
            if error.kind == DriverErrorKind::TargetUnresponsive {
                error.mutation_not_dispatched()
            } else {
                stale_element().mutation_not_dispatched()
            }
        })?;
        if element_signature(&current) != stored.signature {
            return Err(stale_element().mutation_not_dispatched());
        }
        match action {
            SemanticAction::Focus => set_boolean(&stored.element, kAXFocusedAttribute, true),
            SemanticAction::Invoke | SemanticAction::Toggle => {
                perform_native_action(&stored.element, kAXPressAction)
            }
            SemanticAction::SetValue(value) => {
                set_string(&stored.element, kAXValueAttribute, value.expose())
            }
            SemanticAction::SetRangeValue(value) => {
                set_number(&stored.element, kAXValueAttribute, value)
            }
            SemanticAction::Select => set_boolean(&stored.element, kAXSelectedAttribute, true)
                .or_else(|_| perform_native_action(&stored.element, kAXPickAction))
                .or_else(|_| perform_native_action(&stored.element, kAXPressAction)),
            SemanticAction::SetExpanded(expanded) => {
                set_boolean(&stored.element, kAXExpandedAttribute, expanded)
            }
        }
    }
}

fn invoke_at_point(
    pid: i32,
    window_bounds: ScreenRect,
    window_title: &str,
    point: ScreenPoint,
) -> Result<bool, DriverError> {
    ensure_accessibility_permission()?;
    find_window(pid, window_bounds, window_title)?;
    let application = AxElement::application(pid);
    application.set_messaging_timeout(AX_MESSAGING_TIMEOUT_SECONDS)?;
    let mut element = element_at_position(&application, point.x, point.y)?;
    let element_window = copy_element(&element, kAXWindowAttribute)?;
    let (actual_title, actual_bounds) = read_window_identity(&element_window)?;
    if !window_identity_matches(
        actual_bounds,
        actual_title.as_deref(),
        window_bounds,
        window_title,
    ) {
        return Err(stale_element().mutation_not_dispatched());
    }

    // Hit testing may land on a label nested inside an actionable control.
    // Walk a small, bounded ancestor chain without ever crossing the exact
    // revalidated target window.
    for _ in 0..8 {
        if supports_native_action(&element, kAXPressAction)? {
            perform_native_action(&element, kAXPressAction)?;
            return Ok(true);
        }
        let Ok(parent) = copy_element(&element, kAXParentAttribute) else {
            return Ok(false);
        };
        if read_window_identity(&parent)
            .ok()
            .and_then(|(_, bounds)| bounds)
            .is_some_and(|bounds| window_bounds_match(bounds, window_bounds))
        {
            return Ok(false);
        }
        element = parent;
    }
    Ok(false)
}

fn propagate_table_row_names(elements: &mut [DriverElement]) {
    let child_names: HashMap<_, _> = elements
        .iter()
        .filter(|element| !element.name.is_empty())
        .filter_map(|element| {
            element
                .parent_key
                .as_ref()
                .map(|parent| (parent.clone(), element.name.clone()))
        })
        .collect();
    for element in elements {
        if element.role == "row"
            && element.name.is_empty()
            && let Some(name) = child_names.get(&element.key)
        {
            element.name.clone_from(name);
        }
    }
}

fn snapshot_budget(mode: AccessibilityMode) -> SnapshotBudget {
    match mode {
        AccessibilityMode::Disabled => SnapshotBudget {
            max_nodes: 0,
            max_depth: 0,
            max_bytes: 0,
            deadline: Instant::now(),
        },
        AccessibilityMode::Interactive => SnapshotBudget {
            max_nodes: 800,
            max_depth: 16,
            max_bytes: 256 * 1024,
            deadline: Instant::now() + Duration::from_millis(220),
        },
        AccessibilityMode::Full => SnapshotBudget {
            max_nodes: 2_500,
            max_depth: 32,
            max_bytes: 1024 * 1024,
            deadline: Instant::now() + Duration::from_millis(250),
        },
    }
}

fn find_window(
    pid: i32,
    expected_bounds: ScreenRect,
    expected_title: &str,
) -> Result<AxElement, DriverError> {
    let application = AxElement::application(pid);
    application.set_messaging_timeout(AX_MESSAGING_TIMEOUT_SECONDS)?;
    if let Ok(element) = element_at_position(
        &application,
        expected_bounds.x + expected_bounds.width * 0.5,
        expected_bounds.y + expected_bounds.height * 0.5,
    ) && let Ok(window) = copy_element(&element, kAXWindowAttribute)
        && let Ok((title, bounds)) = read_window_identity(&window)
        && window_identity_matches(bounds, title.as_deref(), expected_bounds, expected_title)
    {
        return Ok(window);
    }
    let windows = copy_children(&application, kAXWindowsAttribute)?;
    let mut matching_windows = windows
        .into_iter()
        .filter_map(|window| {
            let (title, bounds) = read_window_identity(&window).ok()?;
            window_identity_matches(bounds, title.as_deref(), expected_bounds, expected_title)
                .then_some(window)
        })
        .take(2);
    let Some(window) = matching_windows.next() else {
        return Err(matching_window_unavailable());
    };
    if matching_windows.next().is_some() {
        return Err(DriverError::new(
            DriverErrorKind::TargetUnavailable,
            "matching accessibility window identity is ambiguous",
        )
        .retryable("observe_window"));
    }
    Ok(window)
}

fn matching_window_unavailable() -> DriverError {
    DriverError::new(
        DriverErrorKind::TargetUnavailable,
        "matching accessibility window is unavailable",
    )
    .retryable("observe_window")
}

#[allow(clippy::cast_possible_truncation)]
fn element_at_position(application: &AxElement, x: f64, y: f64) -> Result<AxElement, DriverError> {
    let mut element: AXUIElementRef = ptr::null_mut();
    // SAFETY: The retained application remains live and success transfers a
    // retained AX element through `element`.
    let error = unsafe {
        AXUIElementCopyElementAtPosition(
            application.as_concrete_TypeRef(),
            x as f32,
            y as f32,
            &raw mut element,
        )
    };
    if error != kAXErrorSuccess || element.is_null() {
        return Err(provider_error(error, "accessibility hit testing failed"));
    }
    // SAFETY: The successful create-rule AX copy returned this non-null value.
    Ok(unsafe { AxElement::wrap_under_create_rule(element) })
}

fn copy_element(element: &AxElement, attribute: &str) -> Result<AxElement, DriverError> {
    copy_attribute(element, attribute)?
        .downcast::<AxElement>()
        .ok_or_else(|| provider_failure("accessibility element relation has an invalid type"))
}

fn copy_attribute(element: &AxElement, attribute: &str) -> Result<CFType, DriverError> {
    let attribute = CFString::new(attribute);
    let mut value: CFTypeRef = ptr::null();
    // SAFETY: The retained element and attribute remain live for the call;
    // success transfers a retained CF object through `value`.
    let error = unsafe {
        AXUIElementCopyAttributeValue(
            element.as_concrete_TypeRef(),
            attribute.as_concrete_TypeRef(),
            &raw mut value,
        )
    };
    if error != kAXErrorSuccess || value.is_null() {
        return Err(provider_error(
            error,
            "accessibility element relation read failed",
        ));
    }
    // SAFETY: A successful create-rule AX copy returned this non-null object.
    Ok(unsafe { CFType::wrap_under_create_rule(value) })
}

fn copy_children(element: &AxElement, attribute: &str) -> Result<Vec<AxElement>, DriverError> {
    let attribute = CFString::new(attribute);
    let mut value: CFTypeRef = ptr::null();
    // SAFETY: The retained element and attribute remain live for the call;
    // success transfers a retained CF object through `value`.
    let error = unsafe {
        AXUIElementCopyAttributeValue(
            element.as_concrete_TypeRef(),
            attribute.as_concrete_TypeRef(),
            &raw mut value,
        )
    };
    if error != kAXErrorSuccess || value.is_null() {
        return Err(provider_error(
            error,
            "accessibility provider child read failed",
        ));
    }
    // SAFETY: A successful create-rule AX copy returned this non-null object.
    let value = unsafe { CFType::wrap_under_create_rule(value) };
    Ok(children_from_value(&value))
}

fn read_window_identity(
    element: &AxElement,
) -> Result<(Option<String>, Option<ScreenRect>), DriverError> {
    let values = copy_multiple(
        element,
        &[kAXTitleAttribute, kAXPositionAttribute, kAXSizeAttribute],
    )?;
    Ok((
        values.first().and_then(cf_string),
        bounds_from_values(values.get(1), values.get(2)),
    ))
}

fn read_node(element: &AxElement) -> Result<NodeValues, DriverError> {
    let values = copy_multiple(
        element,
        &[
            kAXRoleAttribute,
            kAXSubroleAttribute,
            kAXTitleAttribute,
            kAXDescriptionAttribute,
            kAXValueAttribute,
            kAXEnabledAttribute,
            kAXFocusedAttribute,
            kAXPositionAttribute,
            kAXSizeAttribute,
            kAXChildrenAttribute,
            kAXSelectedAttribute,
            kAXExpandedAttribute,
            kAXMinValueAttribute,
            kAXMaxValueAttribute,
            kAXValueIncrementAttribute,
        ],
    )?;
    let role = values.first().and_then(cf_string).unwrap_or_default();
    let subrole = values.get(1).and_then(cf_string).unwrap_or_default();
    let title = values.get(2).and_then(cf_string).unwrap_or_default();
    let description = values.get(3).and_then(cf_string).unwrap_or_default();
    let value = (subrole != kAXSecureTextFieldSubrole)
        .then(|| values.get(4).and_then(cf_string).map(bounded_string))
        .flatten();
    let checked = matches!(role.as_str(), "AXCheckBox" | "AXSwitch" | "AXRadioButton")
        .then(|| values.get(4).and_then(cf_booleanish))
        .flatten();
    let range_value = range_value(
        element,
        values.get(4),
        values.get(12),
        values.get(13),
        values.get(14),
    );
    let value = value.or_else(|| range_value.map(|range| range.value.to_string()));
    Ok(NodeValues {
        role,
        name: bounded_string(if title.is_empty() { description } else { title }),
        value,
        range_value,
        enabled: values.get(5).and_then(cf_boolean).unwrap_or(true),
        focused: values.get(6).and_then(cf_boolean).unwrap_or(false),
        selected: values.get(10).and_then(cf_boolean),
        checked,
        expanded: values.get(11).and_then(cf_boolean),
        bounds: bounds_from_values(values.get(7), values.get(8)),
        children: values.get(9).map(children_from_value).unwrap_or_default(),
    })
}

fn read_action_identity(element: &AxElement) -> Result<NodeValues, DriverError> {
    let values = copy_multiple(
        element,
        &[
            kAXRoleAttribute,
            kAXSubroleAttribute,
            kAXTitleAttribute,
            kAXDescriptionAttribute,
            kAXValueAttribute,
            kAXEnabledAttribute,
            kAXPositionAttribute,
            kAXSizeAttribute,
            kAXMinValueAttribute,
            kAXMaxValueAttribute,
            kAXValueIncrementAttribute,
        ],
    )?;
    let subrole = values.get(1).and_then(cf_string).unwrap_or_default();
    let title = values.get(2).and_then(cf_string).unwrap_or_default();
    let description = values.get(3).and_then(cf_string).unwrap_or_default();
    let value = (subrole != kAXSecureTextFieldSubrole)
        .then(|| values.get(4).and_then(cf_string).map(bounded_string))
        .flatten();
    let range_value = range_value(
        element,
        values.get(4),
        values.get(8),
        values.get(9),
        values.get(10),
    );
    let value = value.or_else(|| range_value.map(|range| range.value.to_string()));
    Ok(NodeValues {
        role: values.first().and_then(cf_string).unwrap_or_default(),
        name: bounded_string(if title.is_empty() { description } else { title }),
        value,
        range_value,
        enabled: values.get(5).and_then(cf_boolean).unwrap_or(true),
        focused: false,
        selected: None,
        checked: None,
        expanded: None,
        bounds: bounds_from_values(values.get(6), values.get(7)),
        children: Vec::new(),
    })
}

fn copy_multiple(element: &AxElement, attributes: &[&str]) -> Result<Vec<CFType>, DriverError> {
    let attributes: Vec<_> = attributes
        .iter()
        .map(|attribute| CFString::new(attribute))
        .collect();
    let attribute_array = CFArray::from_CFTypes(&attributes);
    let mut values: CFArrayRef = ptr::null();
    // SAFETY: `element` and the attribute array remain alive for the call;
    // success transfers a retained CFArray through `values`.
    let error = unsafe {
        AXUIElementCopyMultipleAttributeValues(
            element.as_concrete_TypeRef(),
            attribute_array.as_concrete_TypeRef(),
            0,
            &raw mut values,
        )
    };
    if error != kAXErrorSuccess || values.is_null() {
        return Err(provider_error(
            error,
            "accessibility provider batch read failed",
        ));
    }
    // SAFETY: A successful create-rule AX copy returned this non-null array.
    let values = unsafe { CFArray::<CFType>::wrap_under_create_rule(values) };
    Ok(values.iter().map(|value| value.as_CFType()).collect())
}

fn children_from_value(value: &CFType) -> Vec<AxElement> {
    let Some(array) = value.downcast::<CFArray>() else {
        return Vec::new();
    };
    array
        .iter()
        .filter_map(|pointer| {
            let pointer = *pointer as CFTypeRef;
            if pointer.is_null() {
                return None;
            }
            // SAFETY: The CFArray retains each element for the iteration. The
            // type check precedes wrapping and the wrapper takes its own retain.
            unsafe {
                (CFGetTypeID(pointer) == AXUIElementGetTypeID()).then(|| {
                    AxElement::wrap_under_get_rule(<AxElement as TCFType>::Ref::from_void_ptr(
                        pointer,
                    ))
                })
            }
        })
        .collect()
}

fn cf_string(value: &CFType) -> Option<String> {
    value.downcast::<CFString>().map(|value| value.to_string())
}

fn cf_boolean(value: &CFType) -> Option<bool> {
    value.downcast::<CFBoolean>().map(bool::from)
}

fn cf_booleanish(value: &CFType) -> Option<bool> {
    cf_boolean(value).or_else(|| {
        value
            .downcast::<CFNumber>()
            .and_then(|number| number.to_i64())
            .map(|number| number != 0)
    })
}

fn cf_number(value: &CFType) -> Option<f64> {
    value
        .downcast::<CFNumber>()
        .and_then(|number| number.to_f64())
}

fn range_value(
    element: &AxElement,
    value: Option<&CFType>,
    minimum: Option<&CFType>,
    maximum: Option<&CFType>,
    increment: Option<&CFType>,
) -> Option<AccessibilityRangeValue> {
    let value = value.and_then(cf_number)?;
    let minimum = minimum.and_then(cf_number)?;
    let maximum = maximum.and_then(cf_number)?;
    if !value.is_finite()
        || !minimum.is_finite()
        || !maximum.is_finite()
        || minimum > maximum
        || value < minimum
        || value > maximum
    {
        return None;
    }
    let small_change = increment
        .and_then(cf_number)
        .filter(|value| value.is_finite() && *value > 0.0);
    Some(AccessibilityRangeValue {
        value,
        minimum,
        maximum,
        small_change,
        large_change: None,
        read_only: !attribute_is_settable(element, kAXValueAttribute),
    })
}

fn bounds_from_values(position: Option<&CFType>, size: Option<&CFType>) -> Option<ScreenRect> {
    let position = position.and_then(ax_point)?;
    let size = size.and_then(ax_size)?;
    if !position.x.is_finite()
        || !position.y.is_finite()
        || !size.width.is_finite()
        || !size.height.is_finite()
        || size.width < 0.0
        || size.height < 0.0
    {
        return None;
    }
    Some(ScreenRect {
        x: position.x,
        y: position.y,
        width: size.width,
        height: size.height,
    })
}

fn ax_point(value: &CFType) -> Option<CGPoint> {
    // SAFETY: The CF runtime type and AX value subtype are checked before the
    // API writes into an initialized CGPoint.
    unsafe {
        let raw = value
            .as_CFTypeRef()
            .cast_mut()
            .cast::<accessibility_sys::__AXValue>();
        if CFGetTypeID(value.as_CFTypeRef()) != accessibility_sys::AXValueGetTypeID()
            || AXValueGetType(raw) != kAXValueTypeCGPoint
        {
            return None;
        }
        let mut point = CGPoint::new(0.0, 0.0);
        AXValueGetValue(
            raw,
            kAXValueTypeCGPoint,
            ptr::from_mut(&mut point).cast::<c_void>(),
        )
        .then_some(point)
    }
}

fn ax_size(value: &CFType) -> Option<CGSize> {
    // SAFETY: See `ax_point`; the checked subtype here is CGSize.
    unsafe {
        let raw: AXValueRef = value.as_CFTypeRef().cast_mut().cast();
        if CFGetTypeID(value.as_CFTypeRef()) != accessibility_sys::AXValueGetTypeID()
            || AXValueGetType(raw) != kAXValueTypeCGSize
        {
            return None;
        }
        let mut size = CGSize::new(0.0, 0.0);
        AXValueGetValue(
            raw,
            kAXValueTypeCGSize,
            ptr::from_mut(&mut size).cast::<c_void>(),
        )
        .then_some(size)
    }
}

fn element_signature(values: &NodeValues) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(values.role.as_bytes());
    digest.update([0]);
    digest.update(values.name.as_bytes());
    if let Some(value) = &values.value {
        digest.update([0]);
        digest.update(value.as_bytes());
    }
    if let Some(range) = values.range_value {
        digest.update(range.value.to_bits().to_be_bytes());
        digest.update(range.minimum.to_bits().to_be_bytes());
        digest.update(range.maximum.to_bits().to_be_bytes());
        digest.update([u8::from(range.read_only)]);
    }
    // Keyboard focus can change as the accessibility provider serves the
    // snapshot. It is observable state, but not part of an element's stable
    // identity for action revalidation.
    digest.update([u8::from(values.enabled)]);
    if let Some(bounds) = values.bounds {
        digest.update(bounds.x.to_bits().to_be_bytes());
        digest.update(bounds.y.to_bits().to_be_bytes());
        digest.update(bounds.width.to_bits().to_be_bytes());
        digest.update(bounds.height.to_bits().to_be_bytes());
    }
    digest.finalize().into()
}

fn normalize_role(role: &str) -> String {
    role.strip_prefix("AX").unwrap_or(role).to_ascii_lowercase()
}

fn normalized_actions(role: &str, range_value: Option<AccessibilityRangeValue>) -> Vec<String> {
    if range_value.is_some_and(|range| !range.read_only) {
        return vec!["focus".to_owned(), "set_range_value".to_owned()];
    }
    match role {
        "AXButton" | "AXMenuItem" | "AXLink" => vec!["invoke".to_owned()],
        "AXCheckBox" | "AXSwitch" => vec!["toggle".to_owned()],
        "AXRadioButton" | "AXRow" | "AXCell" | "AXTab" => vec!["select".to_owned()],
        "AXTextField" | "AXTextArea" | "AXComboBox" | "AXSlider" => {
            vec!["focus".to_owned(), "set_value".to_owned()]
        }
        "AXDisclosureTriangle" => vec!["set_expanded".to_owned()],
        _ => Vec::new(),
    }
}

fn bounded_string(mut value: String) -> String {
    if value.len() <= MAX_FIELD_BYTES {
        return value;
    }
    let mut boundary = MAX_FIELD_BYTES;
    while !value.is_char_boundary(boundary) {
        boundary -= 1;
    }
    value.truncate(boundary);
    value
}

fn window_identity_matches(
    bounds: Option<ScreenRect>,
    title: Option<&str>,
    expected_bounds: ScreenRect,
    expected_title: &str,
) -> bool {
    const AX_GEOMETRY_TOLERANCE: f64 = 1.0;

    let Some(bounds) = bounds else {
        return false;
    };
    let geometry_matches = (bounds.x - expected_bounds.x).abs() <= AX_GEOMETRY_TOLERANCE
        && (bounds.y - expected_bounds.y).abs() <= AX_GEOMETRY_TOLERANCE
        && (bounds.width - expected_bounds.width).abs() <= AX_GEOMETRY_TOLERANCE
        && (bounds.height - expected_bounds.height).abs() <= AX_GEOMETRY_TOLERANCE;
    let title_matches =
        expected_title.is_empty() || title.is_none_or(|title| title == expected_title);
    geometry_matches && title_matches
}

fn raise_window(pid: i32, bounds: ScreenRect, title: &str) -> Result<(), DriverError> {
    ensure_accessibility_permission()?;
    let application = AxElement::application(pid);
    application.set_messaging_timeout(AX_MESSAGING_TIMEOUT_SECONDS)?;
    set_boolean(&application, kAXFrontmostAttribute, true).map_err(|error| {
        DriverError::new(
            DriverErrorKind::ForegroundRequired,
            format!(
                "macOS refused target application activation: {}",
                error.message
            ),
        )
        .mutation_not_dispatched()
    })?;
    let window = find_window(pid, bounds, title)?;
    set_boolean(&window, kAXMainAttribute, true).map_err(|error| {
        DriverError::new(
            DriverErrorKind::ForegroundRequired,
            format!("macOS refused target window activation: {}", error.message),
        )
        .mutation_not_dispatched()
    })?;
    perform_native_action(&window, kAXRaiseAction)
}

fn set_window_bounds(
    pid: i32,
    current_bounds: ScreenRect,
    title: &str,
    requested_bounds: ScreenRect,
) -> Result<(), DriverError> {
    ensure_accessibility_permission()?;
    let window = find_window(pid, current_bounds, title)?;
    set_ax_point(
        &window,
        kAXPositionAttribute,
        CGPoint::new(requested_bounds.x, requested_bounds.y),
    )?;
    set_ax_size(
        &window,
        kAXSizeAttribute,
        CGSize::new(requested_bounds.width, requested_bounds.height),
    )?;
    confirm_window_bounds(&window, requested_bounds)
}

fn set_window_minimized(
    pid: i32,
    current_bounds: ScreenRect,
    title: &str,
    minimized: bool,
) -> Result<(), DriverError> {
    ensure_accessibility_permission()?;
    let window = find_window(pid, current_bounds, title)?;
    set_boolean(&window, kAXMinimizedAttribute, minimized)?;
    confirm_window_minimized(&window, minimized)
}

fn confirm_window_bounds(window: &AxElement, requested: ScreenRect) -> Result<(), DriverError> {
    let deadline = Instant::now() + WINDOW_CONFIRM_TIMEOUT;
    loop {
        if read_window_identity(window)
            .ok()
            .and_then(|(_, bounds)| bounds)
            .is_some_and(|actual| window_bounds_match(actual, requested))
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(DriverError::new(
                DriverErrorKind::Platform,
                "macOS adjusted or rejected the requested window bounds",
            )
            .mutation_indeterminate());
        }
        thread::sleep(WINDOW_CONFIRM_INTERVAL);
    }
}

fn confirm_window_minimized(window: &AxElement, minimized: bool) -> Result<(), DriverError> {
    let deadline = Instant::now() + WINDOW_CONFIRM_TIMEOUT;
    loop {
        if copy_attribute(window, kAXMinimizedAttribute)
            .ok()
            .as_ref()
            .and_then(cf_boolean)
            == Some(minimized)
        {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(DriverError::new(
                DriverErrorKind::Platform,
                "macOS did not apply the requested minimized state",
            )
            .mutation_indeterminate());
        }
        thread::sleep(WINDOW_CONFIRM_INTERVAL);
    }
}

fn perform_native_action(element: &AxElement, action: &str) -> Result<(), DriverError> {
    let action = CFString::new(action);
    // SAFETY: Both retained CF objects remain live for the synchronous call.
    let error = unsafe {
        AXUIElementPerformAction(element.as_concrete_TypeRef(), action.as_concrete_TypeRef())
    };
    map_action_error(error)
}

fn supports_native_action(element: &AxElement, expected: &str) -> Result<bool, DriverError> {
    let mut actions: CFArrayRef = ptr::null();
    // SAFETY: The retained element remains live and success transfers a
    // create-rule array of retained CFString values through `actions`.
    let error =
        unsafe { AXUIElementCopyActionNames(element.as_concrete_TypeRef(), &raw mut actions) };
    if error != kAXErrorSuccess || actions.is_null() {
        return Err(provider_error(
            error,
            "accessibility action discovery failed",
        ));
    }
    // SAFETY: A successful create-rule AX copy returned this non-null array.
    let actions = unsafe { CFArray::<CFString>::wrap_under_create_rule(actions) };
    Ok(actions.iter().any(|action| action.to_string() == expected))
}

fn set_boolean(element: &AxElement, attribute: &str, value: bool) -> Result<(), DriverError> {
    set_attribute(element, attribute, CFBoolean::from(value).as_CFTypeRef())
}

fn set_string(element: &AxElement, attribute: &str, value: &str) -> Result<(), DriverError> {
    let value = CFString::new(value);
    set_attribute(element, attribute, value.as_CFTypeRef())
}

fn set_number(element: &AxElement, attribute: &str, value: f64) -> Result<(), DriverError> {
    let value = CFNumber::from(value);
    set_attribute(element, attribute, value.as_CFTypeRef())
}

fn attribute_is_settable(element: &AxElement, attribute: &str) -> bool {
    let attribute = CFString::new(attribute);
    let mut settable = 0_u8;
    // SAFETY: Both retained objects remain live and the API initializes the
    // out-parameter on success.
    let error = unsafe {
        AXUIElementIsAttributeSettable(
            element.as_concrete_TypeRef(),
            attribute.as_concrete_TypeRef(),
            &raw mut settable,
        )
    };
    error == kAXErrorSuccess && settable != 0
}

fn set_ax_point(element: &AxElement, attribute: &str, value: CGPoint) -> Result<(), DriverError> {
    set_ax_value(
        element,
        attribute,
        kAXValueTypeCGPoint,
        ptr::from_ref(&value).cast::<c_void>(),
    )
}

fn set_ax_size(element: &AxElement, attribute: &str, value: CGSize) -> Result<(), DriverError> {
    set_ax_value(
        element,
        attribute,
        kAXValueTypeCGSize,
        ptr::from_ref(&value).cast::<c_void>(),
    )
}

fn set_ax_value(
    element: &AxElement,
    attribute: &str,
    value_type: accessibility_sys::AXValueType,
    value: *const c_void,
) -> Result<(), DriverError> {
    // SAFETY: `value` points to the matching initialized CoreGraphics value for
    // the duration of AXValueCreate. The create-rule result is wrapped exactly
    // once and retained through the synchronous attribute write.
    let value = unsafe { AXValueCreate(value_type, value) };
    if value.is_null() {
        return Err(DriverError::new(
            DriverErrorKind::Platform,
            "macOS failed to encode the requested window geometry",
        )
        .mutation_not_dispatched());
    }
    // SAFETY: AXValueCreate returned a non-null create-rule Core Foundation
    // object and CFType may own its release.
    let value = unsafe { CFType::wrap_under_create_rule(value.cast()) };
    set_attribute(element, attribute, value.as_CFTypeRef())
}

fn window_bounds_match(actual: ScreenRect, requested: ScreenRect) -> bool {
    const TOLERANCE: f64 = 2.0;
    (actual.x - requested.x).abs() <= TOLERANCE
        && (actual.y - requested.y).abs() <= TOLERANCE
        && (actual.width - requested.width).abs() <= TOLERANCE
        && (actual.height - requested.height).abs() <= TOLERANCE
}

fn set_attribute(
    element: &AxElement,
    attribute: &str,
    value: CFTypeRef,
) -> Result<(), DriverError> {
    let attribute = CFString::new(attribute);
    // SAFETY: All three retained CF values remain live for the call.
    let error = unsafe {
        AXUIElementSetAttributeValue(
            element.as_concrete_TypeRef(),
            attribute.as_concrete_TypeRef(),
            value,
        )
    };
    map_action_error(error)
}

fn map_action_error(error: i32) -> Result<(), DriverError> {
    if error == kAXErrorSuccess {
        Ok(())
    } else if error == kAXErrorCannotComplete {
        Err(target_unresponsive("accessibility action timed out").mutation_indeterminate())
    } else {
        Err(DriverError::new(
            DriverErrorKind::StaleObservation,
            "accessibility element no longer accepts the observed action",
        )
        .retryable("observe_window"))
    }
}

fn ensure_accessibility_permission() -> Result<(), DriverError> {
    if macos_accessibility_client::accessibility::application_is_trusted() {
        Ok(())
    } else {
        Err(DriverError::new(
            DriverErrorKind::PermissionRequired,
            "macOS accessibility permission is required",
        )
        .mutation_not_dispatched())
    }
}

fn provider_failure(message: &str) -> DriverError {
    DriverError::new(DriverErrorKind::Platform, message).retryable("retry_accessibility_snapshot")
}

fn provider_error(error: i32, message: &str) -> DriverError {
    if error == kAXErrorCannotComplete {
        target_unresponsive(message)
    } else {
        provider_failure(message)
    }
}

fn target_unresponsive(message: &str) -> DriverError {
    DriverError::new(DriverErrorKind::TargetUnresponsive, message)
        .retryable("retry_after_target_recovers")
}

fn stale_element() -> DriverError {
    DriverError::new(
        DriverErrorKind::StaleObservation,
        "accessibility element changed after observation",
    )
    .retryable("observe_window")
}

fn actor_stopped<T>(_error: T) -> DriverError {
    DriverError::new(DriverErrorKind::Platform, "macOS semantic actor stopped")
}

#[cfg(test)]
mod tests {
    use super::window_identity_matches;
    use nexus_cua_protocol::ScreenRect;

    const EXPECTED: ScreenRect = ScreenRect {
        x: 100.0,
        y: 200.0,
        width: 800.0,
        height: 600.0,
    };

    #[test]
    fn window_identity_requires_matching_geometry() {
        assert!(window_identity_matches(
            Some(EXPECTED),
            Some("Fixture"),
            EXPECTED,
            "Fixture",
        ));
        assert!(window_identity_matches(
            Some(ScreenRect {
                x: 100.5,
                ..EXPECTED
            }),
            Some("Fixture"),
            EXPECTED,
            "Fixture",
        ));
        assert!(!window_identity_matches(
            Some(ScreenRect {
                x: 102.0,
                ..EXPECTED
            }),
            Some("Fixture"),
            EXPECTED,
            "Fixture",
        ));
        assert!(!window_identity_matches(
            None,
            Some("Fixture"),
            EXPECTED,
            "Fixture",
        ));
    }

    #[test]
    fn window_identity_rejects_conflicting_titles() {
        assert!(!window_identity_matches(
            Some(EXPECTED),
            Some("Another Window"),
            EXPECTED,
            "Fixture",
        ));
        assert!(window_identity_matches(
            Some(EXPECTED),
            None,
            EXPECTED,
            "Fixture",
        ));
        assert!(window_identity_matches(
            Some(EXPECTED),
            Some("Any Window"),
            EXPECTED,
            "",
        ));
    }
}
