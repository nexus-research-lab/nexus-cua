//! Bounded `AXUIElement` snapshot and semantic-action actor.

use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::ptr;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread;
use std::time::{Duration, Instant};

use accessibility_sys::{
    AXUIElementCopyMultipleAttributeValues, AXUIElementCreateApplication, AXUIElementGetTypeID,
    AXUIElementPerformAction, AXUIElementRef, AXUIElementSetAttributeValue,
    AXUIElementSetMessagingTimeout, AXValueGetType, AXValueGetValue, AXValueRef,
    kAXChildrenAttribute, kAXDescriptionAttribute, kAXEnabledAttribute, kAXErrorSuccess,
    kAXExpandedAttribute, kAXFocusedAttribute, kAXPickAction, kAXPositionAttribute, kAXPressAction,
    kAXRaiseAction, kAXRoleAttribute, kAXSelectedAttribute, kAXSizeAttribute, kAXTitleAttribute,
    kAXValueAttribute, kAXValueTypeCGPoint, kAXValueTypeCGSize, kAXWindowsAttribute,
};
use core_foundation::array::{CFArray, CFArrayRef};
use core_foundation::base::{CFGetTypeID, CFType, CFTypeRef, TCFType, TCFTypeRef};
use core_foundation::boolean::CFBoolean;
use core_foundation::string::CFString;
use core_foundation::{declare_TCFType, impl_CFTypeDescription, impl_TCFType};
use core_graphics::geometry::{CGPoint, CGSize};
use nexus_cua_protocol::{
    AccessibilityMode, ObservationTruncation, ScreenRect, SensitiveText, TruncationReason,
};
use nexus_cua_runtime::{DriverElement, DriverError, DriverErrorKind};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;

const COMMAND_CAPACITY: usize = 32;
const SNAPSHOT_CACHE_LIMIT: usize = 32;
const AX_MESSAGING_TIMEOUT_SECONDS: f32 = 0.2;
const MAX_FIELD_BYTES: usize = 512;

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
        })?;
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
    RaiseWindow {
        pid: i32,
        window_bounds: ScreenRect,
        window_title: String,
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
    enabled: bool,
    focused: bool,
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
                SemanticCommand::RaiseWindow {
                    pid,
                    window_bounds,
                    window_title,
                    reply,
                } => {
                    let _ = reply.send(raise_window(pid, window_bounds, &window_title));
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
                Err(error) if error.retryable => {
                    truncation = Some(TruncationReason::ProviderFailure);
                    break;
                }
                Err(_) => continue,
            };
            let emit = mode == AccessibilityMode::Full
                || is_interactive(&values.role)
                || !values.name.is_empty()
                || depth <= 1;
            let current_parent = if emit {
                let key = format!("{snapshot_key}:{}", elements.len());
                aggregate_bytes = aggregate_bytes
                    .saturating_add(values.role.len())
                    .saturating_add(values.name.len());
                if aggregate_bytes > budget.max_bytes {
                    truncation = Some(TruncationReason::ByteLimit);
                    break;
                }
                let actions = normalized_actions(&values.role);
                let signature = element_signature(&values);
                elements.push(DriverElement {
                    key: key.clone(),
                    parent_key: nearest_parent.clone(),
                    role: normalize_role(&values.role),
                    name: values.name,
                    value: None,
                    screen_bounds: values.bounds,
                    enabled: values.enabled,
                    focused: values.focused,
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
            .ok_or_else(stale_element)?;
        let current = read_node(&stored.element).map_err(|_| stale_element())?;
        if element_signature(&current) != stored.signature {
            return Err(stale_element());
        }
        match action {
            SemanticAction::Focus => set_boolean(&stored.element, kAXFocusedAttribute, true),
            SemanticAction::Invoke | SemanticAction::Toggle => {
                perform_native_action(&stored.element, kAXPressAction)
            }
            SemanticAction::SetValue(value) => {
                set_string(&stored.element, kAXValueAttribute, value.expose())
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
    let values = copy_multiple(&application, &[kAXWindowsAttribute])?;
    let windows = values.first().map(children_from_value).unwrap_or_default();
    windows
        .into_iter()
        .filter_map(|window| {
            read_window_identity(&window).ok().map(|(title, bounds)| {
                (
                    window,
                    window_score(bounds, title.as_deref(), expected_bounds, expected_title),
                )
            })
        })
        .min_by(|left, right| left.1.total_cmp(&right.1))
        .map(|(window, _)| window)
        .ok_or_else(|| {
            DriverError::new(
                DriverErrorKind::TargetUnavailable,
                "matching accessibility window is unavailable",
            )
            .retryable("observe_window")
        })
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
            kAXTitleAttribute,
            kAXDescriptionAttribute,
            kAXEnabledAttribute,
            kAXFocusedAttribute,
            kAXPositionAttribute,
            kAXSizeAttribute,
            kAXChildrenAttribute,
        ],
    )?;
    let title = values.get(1).and_then(cf_string).unwrap_or_default();
    let description = values.get(2).and_then(cf_string).unwrap_or_default();
    Ok(NodeValues {
        role: values.first().and_then(cf_string).unwrap_or_default(),
        name: bounded_string(if title.is_empty() { description } else { title }),
        enabled: values.get(3).and_then(cf_boolean).unwrap_or(true),
        focused: values.get(4).and_then(cf_boolean).unwrap_or(false),
        bounds: bounds_from_values(values.get(5), values.get(6)),
        children: values.get(7).map(children_from_value).unwrap_or_default(),
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
        return Err(provider_failure("accessibility provider batch read failed"));
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
    digest.update([u8::from(values.enabled), u8::from(values.focused)]);
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

fn normalized_actions(role: &str) -> Vec<String> {
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

fn is_interactive(role: &str) -> bool {
    !normalized_actions(role).is_empty()
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

fn window_score(
    bounds: Option<ScreenRect>,
    title: Option<&str>,
    expected_bounds: ScreenRect,
    expected_title: &str,
) -> f64 {
    let geometry = bounds.map_or(1_000_000.0, |bounds| {
        (bounds.x - expected_bounds.x).abs()
            + (bounds.y - expected_bounds.y).abs()
            + (bounds.width - expected_bounds.width).abs()
            + (bounds.height - expected_bounds.height).abs()
    });
    if !expected_title.is_empty() && title == Some(expected_title) {
        geometry - 10_000.0
    } else {
        geometry
    }
}

fn raise_window(pid: i32, bounds: ScreenRect, title: &str) -> Result<(), DriverError> {
    ensure_accessibility_permission()?;
    perform_native_action(&find_window(pid, bounds, title)?, kAXRaiseAction)
}

fn perform_native_action(element: &AxElement, action: &str) -> Result<(), DriverError> {
    let action = CFString::new(action);
    // SAFETY: Both retained CF objects remain live for the synchronous call.
    let error = unsafe {
        AXUIElementPerformAction(element.as_concrete_TypeRef(), action.as_concrete_TypeRef())
    };
    map_action_error(error)
}

fn set_boolean(element: &AxElement, attribute: &str, value: bool) -> Result<(), DriverError> {
    set_attribute(element, attribute, CFBoolean::from(value).as_CFTypeRef())
}

fn set_string(element: &AxElement, attribute: &str, value: &str) -> Result<(), DriverError> {
    let value = CFString::new(value);
    set_attribute(element, attribute, value.as_CFTypeRef())
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
        ))
    }
}

fn provider_failure(message: &str) -> DriverError {
    DriverError::new(DriverErrorKind::Platform, message).retryable("retry_accessibility_snapshot")
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
