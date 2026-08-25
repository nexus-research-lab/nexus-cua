//! Cached Windows UI Automation snapshot and semantic-action MTA actor.

use std::collections::{HashMap, VecDeque};
use std::ffi::c_void;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use std::thread;
use std::time::{Duration, Instant};

use nexus_cua_protocol::{
    AccessibilityMode, ObservationTruncation, ScreenRect, SensitiveText, TruncationReason,
};
use nexus_cua_runtime::{DriverElement, DriverError, DriverErrorKind};
use sha2::{Digest, Sha256};
use tokio::sync::oneshot;
use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED, CoCreateInstance, CoInitializeEx, CoUninitialize,
};
use windows::Win32::UI::Accessibility::{
    AutomationElementMode_Full, CUIAutomation, IUIAutomation, IUIAutomationCacheRequest,
    IUIAutomationElement, IUIAutomationElementArray, IUIAutomationExpandCollapsePattern,
    IUIAutomationInvokePattern, IUIAutomationSelectionItemPattern, IUIAutomationTogglePattern,
    IUIAutomationValuePattern, TreeScope_Element, TreeScope_Subtree,
    UIA_BoundingRectanglePropertyId, UIA_ButtonControlTypeId, UIA_CheckBoxControlTypeId,
    UIA_ComboBoxControlTypeId, UIA_ControlTypePropertyId, UIA_CustomControlTypeId,
    UIA_DataGridControlTypeId, UIA_DataItemControlTypeId, UIA_DocumentControlTypeId,
    UIA_EditControlTypeId, UIA_ExpandCollapsePatternId, UIA_GroupControlTypeId,
    UIA_HasKeyboardFocusPropertyId, UIA_HeaderControlTypeId, UIA_HeaderItemControlTypeId,
    UIA_HyperlinkControlTypeId, UIA_ImageControlTypeId, UIA_InvokePatternId,
    UIA_IsEnabledPropertyId, UIA_IsKeyboardFocusablePropertyId, UIA_IsOffscreenPropertyId,
    UIA_IsPasswordPropertyId, UIA_ListControlTypeId, UIA_ListItemControlTypeId,
    UIA_MenuBarControlTypeId, UIA_MenuControlTypeId, UIA_MenuItemControlTypeId, UIA_NamePropertyId,
    UIA_PaneControlTypeId, UIA_ProgressBarControlTypeId, UIA_RadioButtonControlTypeId,
    UIA_ScrollBarControlTypeId, UIA_SelectionItemPatternId, UIA_SemanticZoomControlTypeId,
    UIA_SeparatorControlTypeId, UIA_SliderControlTypeId, UIA_SpinnerControlTypeId,
    UIA_SplitButtonControlTypeId, UIA_StatusBarControlTypeId, UIA_TabControlTypeId,
    UIA_TabItemControlTypeId, UIA_TableControlTypeId, UIA_TextControlTypeId,
    UIA_ThumbControlTypeId, UIA_TitleBarControlTypeId, UIA_TogglePatternId,
    UIA_ToolBarControlTypeId, UIA_ToolTipControlTypeId, UIA_TreeControlTypeId,
    UIA_TreeItemControlTypeId, UIA_ValuePatternId, UIA_WindowControlTypeId,
};
use windows::core::{BOOL, BSTR};

const COMMAND_CAPACITY: usize = 32;
const SNAPSHOT_CACHE_LIMIT: usize = 32;
const MAX_FIELD_BYTES: usize = 512;

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
    pub(super) fn spawn() -> Result<Self, DriverError> {
        let (sender, receiver) = sync_channel(COMMAND_CAPACITY);
        let (ready, ready_receiver) = sync_channel(1);
        thread::Builder::new()
            .name("nexus-cua-windows-uia-mta".to_owned())
            .spawn(move || match SemanticState::new() {
                Ok(state) => {
                    let _ = ready.send(Ok(()));
                    state.run(&receiver);
                }
                Err(error) => {
                    let _ = ready.send(Err(error));
                }
            })
            .map_err(|_| provider_failure("failed to create UI Automation MTA actor"))?;
        ready_receiver
            .recv()
            .map_err(|_| provider_failure("UI Automation actor stopped during startup"))??;
        Ok(Self { sender })
    }

    pub(super) async fn snapshot(
        &self,
        hwnd: isize,
        mode: AccessibilityMode,
    ) -> Result<SemanticSnapshot, DriverError> {
        let (reply, receiver) = oneshot::channel();
        self.admit(SemanticCommand::Snapshot { hwnd, mode, reply })?;
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

    fn admit(&self, command: SemanticCommand) -> Result<(), DriverError> {
        self.sender.try_send(command).map_err(|error| match error {
            TrySendError::Full(_) => {
                DriverError::new(DriverErrorKind::Busy, "Windows semantic actor is busy")
                    .retryable("retry_with_backoff")
            }
            TrySendError::Disconnected(_) => actor_stopped(()),
        })
    }
}

enum SemanticCommand {
    Snapshot {
        hwnd: isize,
        mode: AccessibilityMode,
        reply: oneshot::Sender<Result<SemanticSnapshot, DriverError>>,
    },
    Perform {
        element_key: String,
        action: SemanticAction,
        reply: oneshot::Sender<Result<(), DriverError>>,
    },
}

struct SemanticState {
    automation: IUIAutomation,
    interactive_request: IUIAutomationCacheRequest,
    full_request: IUIAutomationCacheRequest,
    element_request: IUIAutomationCacheRequest,
    next_snapshot: u64,
    snapshots: VecDeque<StoredSnapshot>,
}

struct StoredSnapshot {
    elements: HashMap<String, StoredElement>,
}

struct StoredElement {
    element: IUIAutomationElement,
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
    state: NodeState,
    bounds: Option<ScreenRect>,
}

#[derive(Clone, Copy)]
struct NodeState(u8);

impl NodeState {
    const ENABLED: u8 = 1 << 0;
    const FOCUSED: u8 = 1 << 1;
    const FOCUSABLE: u8 = 1 << 2;
    const OFFSCREEN: u8 = 1 << 3;

    fn from_flags([enabled, focused, focusable, offscreen]: [bool; 4]) -> Self {
        Self(
            (u8::from(enabled) * Self::ENABLED)
                | (u8::from(focused) * Self::FOCUSED)
                | (u8::from(focusable) * Self::FOCUSABLE)
                | (u8::from(offscreen) * Self::OFFSCREEN),
        )
    }

    fn contains(self, flag: u8) -> bool {
        self.0 & flag != 0
    }

    fn enabled(self) -> bool {
        self.contains(Self::ENABLED)
    }

    fn focused(self) -> bool {
        self.contains(Self::FOCUSED)
    }

    fn focusable(self) -> bool {
        self.contains(Self::FOCUSABLE)
    }

    fn offscreen(self) -> bool {
        self.contains(Self::OFFSCREEN)
    }
}

impl SemanticState {
    fn new() -> Result<Self, DriverError> {
        // SAFETY: This dedicated actor owns one COM MTA initialization for its
        // complete thread lifetime and balances it in Drop on the same thread.
        unsafe {
            CoInitializeEx(None, COINIT_MULTITHREADED)
                .ok()
                .map_err(|_| provider_failure("failed to initialize COM MTA"))?;
            let automation: IUIAutomation =
                CoCreateInstance(&CUIAutomation, None, CLSCTX_INPROC_SERVER)
                    .map_err(|_| provider_failure("failed to create UI Automation client"))?;
            let interactive_request = cache_request(&automation, false, TreeScope_Subtree)?;
            let full_request = cache_request(&automation, true, TreeScope_Subtree)?;
            let element_request = cache_request(&automation, true, TreeScope_Element)?;
            Ok(Self {
                automation,
                interactive_request,
                full_request,
                element_request,
                next_snapshot: 0,
                snapshots: VecDeque::new(),
            })
        }
    }

    fn run(mut self, receiver: &Receiver<SemanticCommand>) {
        while let Ok(command) = receiver.recv() {
            match command {
                SemanticCommand::Snapshot { hwnd, mode, reply } => {
                    let _ = reply.send(self.snapshot(hwnd, mode));
                }
                SemanticCommand::Perform {
                    element_key,
                    action,
                    reply,
                } => {
                    let _ = reply.send(self.perform(&element_key, action));
                }
            }
        }
    }

    fn snapshot(
        &mut self,
        raw_hwnd: isize,
        mode: AccessibilityMode,
    ) -> Result<SemanticSnapshot, DriverError> {
        let request = if mode == AccessibilityMode::Full {
            &self.full_request
        } else {
            &self.interactive_request
        };
        // SAFETY: UIA and HWND use only this COM MTA actor. The cache request
        // gathers the subtree in one provider transaction before traversal.
        let root = unsafe {
            self.automation
                .ElementFromHandleBuildCache(hwnd(raw_hwnd), request)
                .map_err(|_| provider_failure("UI Automation cache build failed"))?
        };
        self.next_snapshot = self.next_snapshot.wrapping_add(1);
        let snapshot_key = format!("uia:{raw_hwnd:x}:{}", self.next_snapshot);
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
            let Ok(values) = cached_values(&element) else {
                truncation = Some(TruncationReason::ProviderFailure);
                break;
            };
            let actions = normalized_actions(&values.role);
            let emit = mode == AccessibilityMode::Full
                || !actions.is_empty()
                || values.state.focusable()
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
                let signature = element_signature(&values);
                elements.push(DriverElement {
                    key: key.clone(),
                    parent_key: nearest_parent.clone(),
                    role: values.role,
                    name: values.name,
                    value: None,
                    screen_bounds: values.bounds,
                    enabled: values.state.enabled(),
                    focused: values.state.focused(),
                    actions,
                });
                stored.insert(
                    key.clone(),
                    StoredElement {
                        element: element.clone(),
                        signature,
                    },
                );
                Some(key)
            } else {
                nearest_parent
            };
            if !values.state.offscreen() || mode == AccessibilityMode::Full {
                for child in cached_children(&element) {
                    queue.push_back((child, current_parent.clone(), depth + 1));
                }
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
        let stored = self
            .snapshots
            .iter()
            .rev()
            .find_map(|snapshot| snapshot.elements.get(element_key))
            .ok_or_else(stale_element)?;
        // SAFETY: BuildUpdatedCache and every pattern call stay on the owning
        // COM MTA. The refreshed signature closes stale-element races.
        unsafe {
            let current = stored
                .element
                .BuildUpdatedCache(&self.element_request)
                .map_err(|_| stale_element())?;
            if element_signature(&cached_values(&current)?) != stored.signature {
                return Err(stale_element());
            }
            (|| -> windows::core::Result<()> {
                match action {
                    SemanticAction::Focus => current.SetFocus(),
                    SemanticAction::Invoke => current
                        .GetCurrentPatternAs::<IUIAutomationInvokePattern>(UIA_InvokePatternId)?
                        .Invoke(),
                    SemanticAction::SetValue(value) => current
                        .GetCurrentPatternAs::<IUIAutomationValuePattern>(UIA_ValuePatternId)?
                        .SetValue(&BSTR::from(value.expose())),
                    SemanticAction::Toggle => current
                        .GetCurrentPatternAs::<IUIAutomationTogglePattern>(UIA_TogglePatternId)?
                        .Toggle(),
                    SemanticAction::Select => current
                        .GetCurrentPatternAs::<IUIAutomationSelectionItemPattern>(
                            UIA_SelectionItemPatternId,
                        )?
                        .Select(),
                    SemanticAction::SetExpanded(expanded) => {
                        let pattern = current
                            .GetCurrentPatternAs::<IUIAutomationExpandCollapsePattern>(
                                UIA_ExpandCollapsePatternId,
                            )?;
                        if expanded {
                            pattern.Expand()
                        } else {
                            pattern.Collapse()
                        }
                    }
                }
            })()
            .map_err(|_| stale_element())
        }
    }
}

impl Drop for SemanticState {
    fn drop(&mut self) {
        // SAFETY: Balances the successful CoInitializeEx on this actor thread.
        unsafe { CoUninitialize() };
    }
}

unsafe fn cache_request(
    automation: &IUIAutomation,
    raw_view: bool,
    scope: windows::Win32::UI::Accessibility::TreeScope,
) -> Result<IUIAutomationCacheRequest, DriverError> {
    // SAFETY: Every COM call is issued on the freshly initialized MTA and all
    // referenced interface objects remain alive in the resulting state.
    unsafe {
        let request = automation
            .CreateCacheRequest()
            .map_err(|_| provider_failure("failed to create UIA cache request"))?;
        request
            .SetAutomationElementMode(AutomationElementMode_Full)
            .map_err(|_| provider_failure("failed to configure UIA element mode"))?;
        request
            .SetTreeScope(scope)
            .map_err(|_| provider_failure("failed to configure UIA tree scope"))?;
        let filter = if raw_view {
            automation.RawViewCondition()
        } else {
            automation.ControlViewCondition()
        }
        .map_err(|_| provider_failure("failed to select UIA tree view"))?;
        request
            .SetTreeFilter(&filter)
            .map_err(|_| provider_failure("failed to configure UIA tree filter"))?;
        for property in [
            UIA_NamePropertyId,
            UIA_ControlTypePropertyId,
            UIA_BoundingRectanglePropertyId,
            UIA_IsEnabledPropertyId,
            UIA_HasKeyboardFocusPropertyId,
            UIA_IsKeyboardFocusablePropertyId,
            UIA_IsOffscreenPropertyId,
            UIA_IsPasswordPropertyId,
        ] {
            request
                .AddProperty(property)
                .map_err(|_| provider_failure("failed to add UIA cached property"))?;
        }
        Ok(request)
    }
}

fn cached_values(element: &IUIAutomationElement) -> Result<NodeValues, DriverError> {
    // SAFETY: These reads access only the immutable cache created on this MTA;
    // they do not make per-property provider calls.
    unsafe {
        let control_type = element
            .CachedControlType()
            .map_err(|_| provider_failure("cached UIA control type is unavailable"))?;
        let name = element.CachedName().unwrap_or_default().to_string();
        let rectangle = element.CachedBoundingRectangle().unwrap_or_default();
        Ok(NodeValues {
            role: normalize_control_type(control_type),
            name: bounded_string(name),
            state: NodeState::from_flags([
                element.CachedIsEnabled().map_or(true, BOOL::as_bool),
                element.CachedHasKeyboardFocus().is_ok_and(BOOL::as_bool),
                element.CachedIsKeyboardFocusable().is_ok_and(BOOL::as_bool),
                element.CachedIsOffscreen().is_ok_and(BOOL::as_bool),
            ]),
            bounds: screen_rect(rectangle),
        })
    }
}

fn cached_children(element: &IUIAutomationElement) -> Vec<IUIAutomationElement> {
    // SAFETY: Reads only the subtree captured by the cache request.
    unsafe {
        element
            .GetCachedChildren()
            .ok()
            .map_or_else(Vec::new, |array| elements_from_array(&array))
    }
}

fn elements_from_array(array: &IUIAutomationElementArray) -> Vec<IUIAutomationElement> {
    // SAFETY: Length bounds the indexed COM reads and all calls remain on MTA.
    unsafe {
        let length = array.Length().unwrap_or(0).max(0);
        (0..length)
            .filter_map(|index| array.GetElement(index).ok())
            .collect()
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
            max_nodes: 1_000,
            max_depth: 20,
            max_bytes: 256 * 1024,
            deadline: Instant::now() + Duration::from_millis(220),
        },
        AccessibilityMode::Full => SnapshotBudget {
            max_nodes: 3_000,
            max_depth: 40,
            max_bytes: 1024 * 1024,
            deadline: Instant::now() + Duration::from_millis(250),
        },
    }
}

fn screen_rect(rectangle: RECT) -> Option<ScreenRect> {
    let width = rectangle.right.saturating_sub(rectangle.left);
    let height = rectangle.bottom.saturating_sub(rectangle.top);
    (width >= 0 && height >= 0).then_some(ScreenRect {
        x: f64::from(rectangle.left),
        y: f64::from(rectangle.top),
        width: f64::from(width),
        height: f64::from(height),
    })
}

fn normalize_control_type(
    control_type: windows::Win32::UI::Accessibility::UIA_CONTROLTYPE_ID,
) -> String {
    let role = if control_type == UIA_ButtonControlTypeId {
        "button"
    } else if control_type == UIA_CheckBoxControlTypeId {
        "checkbox"
    } else if control_type == UIA_ComboBoxControlTypeId {
        "combobox"
    } else if control_type == UIA_EditControlTypeId {
        "edit"
    } else if control_type == UIA_HyperlinkControlTypeId {
        "link"
    } else if control_type == UIA_ListControlTypeId {
        "list"
    } else if control_type == UIA_ListItemControlTypeId {
        "listitem"
    } else if control_type == UIA_MenuControlTypeId {
        "menu"
    } else if control_type == UIA_MenuBarControlTypeId {
        "menubar"
    } else if control_type == UIA_MenuItemControlTypeId {
        "menuitem"
    } else if control_type == UIA_RadioButtonControlTypeId {
        "radio"
    } else if control_type == UIA_SliderControlTypeId {
        "slider"
    } else if control_type == UIA_TabControlTypeId {
        "tablist"
    } else if control_type == UIA_TabItemControlTypeId {
        "tab"
    } else if control_type == UIA_TreeControlTypeId {
        "tree"
    } else if control_type == UIA_TreeItemControlTypeId {
        "treeitem"
    } else if control_type == UIA_WindowControlTypeId {
        "window"
    } else if control_type == UIA_TextControlTypeId {
        "text"
    } else if control_type == UIA_DocumentControlTypeId {
        "document"
    } else if control_type == UIA_ImageControlTypeId {
        "image"
    } else if control_type == UIA_GroupControlTypeId {
        "group"
    } else if control_type == UIA_PaneControlTypeId {
        "pane"
    } else if control_type == UIA_DataGridControlTypeId {
        "datagrid"
    } else if control_type == UIA_DataItemControlTypeId {
        "dataitem"
    } else if control_type == UIA_TableControlTypeId {
        "table"
    } else if control_type == UIA_HeaderControlTypeId {
        "header"
    } else if control_type == UIA_HeaderItemControlTypeId {
        "headeritem"
    } else if control_type == UIA_ScrollBarControlTypeId {
        "scrollbar"
    } else if control_type == UIA_ProgressBarControlTypeId {
        "progressbar"
    } else if control_type == UIA_SpinnerControlTypeId {
        "spinner"
    } else if control_type == UIA_SplitButtonControlTypeId {
        "splitbutton"
    } else if control_type == UIA_StatusBarControlTypeId {
        "statusbar"
    } else if control_type == UIA_ToolBarControlTypeId {
        "toolbar"
    } else if control_type == UIA_ToolTipControlTypeId {
        "tooltip"
    } else if control_type == UIA_TitleBarControlTypeId {
        "titlebar"
    } else if control_type == UIA_ThumbControlTypeId {
        "thumb"
    } else if control_type == UIA_SeparatorControlTypeId {
        "separator"
    } else if control_type == UIA_SemanticZoomControlTypeId {
        "semanticzoom"
    } else if control_type == UIA_CustomControlTypeId {
        "custom"
    } else {
        "unknown"
    };
    role.to_owned()
}

fn normalized_actions(role: &str) -> Vec<String> {
    match role {
        "button" | "link" | "menuitem" | "splitbutton" => vec!["invoke".to_owned()],
        "checkbox" => vec!["toggle".to_owned()],
        "radio" | "listitem" | "tab" | "dataitem" => vec!["select".to_owned()],
        "edit" | "combobox" | "slider" | "spinner" => {
            vec!["focus".to_owned(), "set_value".to_owned()]
        }
        "treeitem" => vec!["select".to_owned(), "set_expanded".to_owned()],
        _ => Vec::new(),
    }
}

fn element_signature(values: &NodeValues) -> [u8; 32] {
    let mut digest = Sha256::new();
    digest.update(values.role.as_bytes());
    digest.update([0]);
    digest.update(values.name.as_bytes());
    digest.update([values.state.0]);
    if let Some(bounds) = values.bounds {
        digest.update(bounds.x.to_bits().to_be_bytes());
        digest.update(bounds.y.to_bits().to_be_bytes());
        digest.update(bounds.width.to_bits().to_be_bytes());
        digest.update(bounds.height.to_bits().to_be_bytes());
    }
    digest.finalize().into()
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

fn hwnd(raw: isize) -> HWND {
    HWND(raw as *mut c_void)
}

fn provider_failure(message: &str) -> DriverError {
    DriverError::new(DriverErrorKind::Platform, message).retryable("retry_uia_snapshot")
}

fn stale_element() -> DriverError {
    DriverError::new(
        DriverErrorKind::StaleObservation,
        "UI Automation element changed after observation",
    )
    .retryable("observe_window")
}

fn actor_stopped<T>(_error: T) -> DriverError {
    DriverError::new(DriverErrorKind::Platform, "Windows semantic actor stopped")
}
