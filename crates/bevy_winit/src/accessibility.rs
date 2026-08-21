//! Helpers for mapping window entities to accessibility types

use alloc::{collections::VecDeque, sync::Arc};
use bevy_input_focus::InputFocus;
#[cfg(test)]
use core::cell::Cell;
use core::cell::RefCell;
use std::sync::Mutex;
use tracing::warn;
use winit::event_loop::{ActiveEventLoop, EventLoopProxy};

use accesskit::{
    ActionHandler, ActionRequest, ActivationHandler, DeactivationHandler, Node, NodeId, Role, Tree,
    TreeId, TreeUpdate,
};
use accesskit_winit::Adapter;
use bevy_a11y::{
    AccessibilityActivationGeneration, AccessibilityAdapterGeneration,
    AccessibilityAttachmentGeneration, AccessibilityNode, AccessibilityRequested,
    AccessibilitySubtreeActionRequest, AccessibilitySubtreeLifecycle,
    AccessibilitySubtreeLifecycleToken, AccessibilitySubtreeOrder, AccessibilitySubtreeParentFocus,
    AccessibilitySubtreeProvider, AccessibilitySubtreeSnapshot, AccessibilitySubtreeTransform,
    AccessibilitySystems, ActionRequest as ActionRequestWrapper, ManageAccessibilityUpdates,
    WindowAccessibilityState,
};
use bevy_app::{App, Last, Plugin, PostUpdate};
use bevy_derive::{Deref, DerefMut};
use bevy_ecs::{
    entity::{EntityHashMap, EntityHashSet},
    prelude::*,
    system::NonSendMarker,
};
use bevy_platform::collections::{HashMap, HashSet};
use bevy_window::{PrimaryWindow, RequestRedraw, Window, WindowClosed};

use crate::WinitUserEvent;

thread_local! {
    /// Temporary storage of access kit adapter data to replace usage of `!Send` resources. This will be replaced with proper
    /// storage of `!Send` data after issue #17667 is complete.
    pub static ACCESS_KIT_ADAPTERS: RefCell<AccessKitAdapters> = const { RefCell::new(AccessKitAdapters::new()) };
    #[cfg(test)]
    static SNAPSHOT_MATERIALIZATIONS: Cell<usize> = const { Cell::new(0) };
}

/// Maps window entities to their `AccessKit` [`Adapter`]s.
#[derive(Default, Deref, DerefMut)]
pub struct AccessKitAdapters(pub EntityHashMap<AccessKitAdapter>);

impl AccessKitAdapters {
    /// Creates a new empty `AccessKitAdapters`.
    pub const fn new() -> Self {
        Self(EntityHashMap::new())
    }
}

/// Maps window entities to their respective [`ActionRequest`]s.
#[derive(Resource, Default)]
pub struct WinitActionRequestHandlers {
    handlers: EntityHashMap<Arc<Mutex<WinitAccessibilityRuntime>>>,
    next_adapter_generation: u64,
    pending_lifecycle: Vec<AccessibilitySubtreeLifecycle>,
}

/// Forwards `AccessKit` [`ActionRequest`]s from winit to an event channel.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RouteToken {
    provider: Entity,
    attachment_generation: AccessibilityAttachmentGeneration,
    snapshot_sequence: u64,
    committed_transform: Option<AccessibilitySubtreeTransform>,
}

impl RouteToken {
    fn same_attachment_snapshot(&self, other: &Self) -> bool {
        self.provider == other.provider
            && self.attachment_generation == other.attachment_generation
            && self.snapshot_sequence == other.snapshot_sequence
    }
}

#[derive(Clone, Debug)]
struct QueuedAction {
    adapter_generation: AccessibilityAdapterGeneration,
    activation_generation: AccessibilityActivationGeneration,
    route: Option<RouteToken>,
    request: ActionRequest,
}

struct WinitAccessibilityRuntime {
    adapter_generation: AccessibilityAdapterGeneration,
    active: bool,
    activation_generation: u64,
    pending_deactivation_generations: VecDeque<AccessibilityActivationGeneration>,
    routes: HashMap<TreeId, RouteToken>,
    actions: VecDeque<QueuedAction>,
    wake_callback: Option<Arc<dyn Fn() -> bool + Send + Sync>>,
    produced_callback_epoch: u64,
    consumed_callback_epoch: u64,
    wake_sent_epoch: u64,
    owner_observed_epoch: Option<u64>,
}

impl Default for WinitAccessibilityRuntime {
    fn default() -> Self {
        Self {
            adapter_generation: AccessibilityAdapterGeneration(0),
            active: false,
            activation_generation: 0,
            pending_deactivation_generations: VecDeque::new(),
            routes: HashMap::default(),
            actions: VecDeque::new(),
            wake_callback: None,
            produced_callback_epoch: 0,
            consumed_callback_epoch: 0,
            wake_sent_epoch: 0,
            owner_observed_epoch: None,
        }
    }
}

impl WinitAccessibilityRuntime {
    fn produce_callback(&mut self) {
        self.produced_callback_epoch = self
            .produced_callback_epoch
            .checked_add(1)
            .expect("accessibility callback epoch exhausted");
        self.ensure_wake();
    }

    fn begin_owner_pass(&mut self) {
        self.owner_observed_epoch = Some(self.produced_callback_epoch);
    }

    fn acknowledge_owner_pass(&mut self) {
        let Some(observed) = self.owner_observed_epoch.take() else {
            return;
        };
        self.consumed_callback_epoch = self.consumed_callback_epoch.max(observed);
        self.ensure_wake();
    }

    fn ensure_wake(&mut self) {
        if self.produced_callback_epoch == self.consumed_callback_epoch
            || self.wake_sent_epoch > self.consumed_callback_epoch
        {
            return;
        }
        let Some(wake) = &self.wake_callback else {
            return;
        };
        if wake() {
            self.wake_sent_epoch = self.produced_callback_epoch;
        }
    }
}

struct AttachedProvider {
    tree_id: TreeId,
    attachment_generation: AccessibilityAttachmentGeneration,
    snapshot_sequence: Option<u64>,
    snapshot: Option<AccessibilitySubtreeSnapshot>,
    notified_activation: Option<AccessibilityActivationGeneration>,
    published_activation: Option<AccessibilityActivationGeneration>,
    published_sequence: Option<u64>,
    detached: bool,
    duplicate_invalid: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum ProviderLifecycleEmission {
    Activated {
        activation_generation: AccessibilityActivationGeneration,
        attachment_generation: AccessibilityAttachmentGeneration,
    },
    Deactivated {
        activation_generation: AccessibilityActivationGeneration,
        attachment_generation: AccessibilityAttachmentGeneration,
    },
}

/// One native AccessKit adapter and its retained composition state.
pub struct AccessKitAdapter {
    adapter: Adapter,
    generation: AccessibilityAdapterGeneration,
    runtime: Arc<Mutex<WinitAccessibilityRuntime>>,
    window_state: WindowAccessibilityState,
    providers: EntityHashMap<AttachedProvider>,
    next_attachment_generation: u64,
    last_root_activation: Option<AccessibilityActivationGeneration>,
    last_root: Option<TreeUpdate>,
    focus_conflict_active: bool,
    invalid_grafts: EntityHashSet,
}

#[derive(Debug, PartialEq)]
struct ProviderCertificate {
    entity: Entity,
    provider: AccessibilitySubtreeProvider,
    provider_tick: u32,
    snapshot_sequence: Option<u64>,
    snapshot_tick: Option<u32>,
    order: AccessibilitySubtreeOrder,
    order_tick: Option<u32>,
    transform_bits: Option<[u64; 6]>,
    transform_tick: Option<u32>,
    parent_focused: bool,
    parent_focus_tick: Option<u32>,
}

#[derive(Resource, Default)]
struct AccessibilityCompositionBoundary(Vec<ProviderCertificate>);

#[derive(Resource, Default)]
struct AccessibilityConsumerFollowUp(bool);

impl core::ops::Deref for AccessKitAdapter {
    type Target = Adapter;

    fn deref(&self) -> &Self::Target {
        &self.adapter
    }
}

impl core::ops::DerefMut for AccessKitAdapter {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.adapter
    }
}

struct AccessKitState {
    name: String,
    entity: Entity,
    requested: AccessibilityRequested,
    window_state: WindowAccessibilityState,
    runtime: Arc<Mutex<WinitAccessibilityRuntime>>,
}

impl AccessKitState {
    fn new(
        name: impl Into<String>,
        entity: Entity,
        requested: AccessibilityRequested,
        window_state: WindowAccessibilityState,
        runtime: Arc<Mutex<WinitAccessibilityRuntime>>,
    ) -> Arc<Mutex<Self>> {
        let name = name.into();

        Arc::new(Mutex::new(Self {
            name,
            entity,
            requested,
            window_state,
            runtime,
        }))
    }

    fn build_root(&mut self) -> Node {
        let mut node = Node::new(Role::Window);
        node.set_label(self.name.clone());
        node
    }

    fn build_initial_tree(&mut self) -> TreeUpdate {
        let root = self.build_root();
        let accesskit_window_id = NodeId(self.entity.to_bits());
        let tree = Tree::new(accesskit_window_id);
        self.requested.set(true);
        self.window_state.set_platform_active(true);
        {
            let mut runtime = self.runtime.lock().unwrap();
            if runtime.active {
                let previous = AccessibilityActivationGeneration(runtime.activation_generation);
                runtime.pending_deactivation_generations.push_back(previous);
            }
            runtime.active = true;
            runtime.activation_generation = runtime.activation_generation.wrapping_add(1);
            runtime.routes.clear();
            runtime.produce_callback();
        }
        TreeUpdate {
            nodes: vec![(accesskit_window_id, root)],
            tree: Some(tree),
            tree_id: TreeId::ROOT,
            focus: accesskit_window_id,
        }
    }
}

struct WinitActivationHandler(Arc<Mutex<AccessKitState>>);

impl ActivationHandler for WinitActivationHandler {
    fn request_initial_tree(&mut self) -> Option<TreeUpdate> {
        Some(self.0.lock().unwrap().build_initial_tree())
    }
}

impl WinitActivationHandler {
    pub fn new(state: Arc<Mutex<AccessKitState>>) -> Self {
        Self(state)
    }
}

#[derive(Clone)]
struct WinitActionHandler {
    generation: AccessibilityAdapterGeneration,
    runtime: Arc<Mutex<WinitAccessibilityRuntime>>,
}

impl ActionHandler for WinitActionHandler {
    fn do_action(&mut self, request: ActionRequest) {
        let mut runtime = self.runtime.lock().unwrap();
        let activation_generation =
            AccessibilityActivationGeneration(runtime.activation_generation);
        let route = if request.target_tree == TreeId::ROOT {
            None
        } else {
            runtime.routes.get(&request.target_tree).copied()
        };
        if runtime.active && (request.target_tree == TreeId::ROOT || route.is_some()) {
            runtime.actions.push_back(QueuedAction {
                adapter_generation: self.generation,
                activation_generation,
                route,
                request,
            });
            runtime.produce_callback();
        }
    }
}

impl WinitActionHandler {
    pub fn new(
        generation: AccessibilityAdapterGeneration,
        runtime: Arc<Mutex<WinitAccessibilityRuntime>>,
    ) -> Self {
        Self {
            generation,
            runtime,
        }
    }
}

struct WinitDeactivationHandler {
    window_state: WindowAccessibilityState,
    runtime: Arc<Mutex<WinitAccessibilityRuntime>>,
}

impl DeactivationHandler for WinitDeactivationHandler {
    fn deactivate_accessibility(&mut self) {
        let mut runtime = self.runtime.lock().unwrap();
        if runtime.active {
            let previous = AccessibilityActivationGeneration(runtime.activation_generation);
            runtime.pending_deactivation_generations.push_back(previous);
        }
        runtime.active = false;
        runtime.routes.clear();
        runtime.actions.clear();
        self.window_state.set_platform_active(false);
        runtime.produce_callback();
    }
}

/// Prepares accessibility for a winit window.
pub(crate) fn prepare_accessibility_for_window(
    event_loop: &ActiveEventLoop,
    winit_window: &winit::window::Window,
    entity: Entity,
    name: String,
    accessibility_requested: AccessibilityRequested,
    window_state: WindowAccessibilityState,
    event_loop_proxy: EventLoopProxy<WinitUserEvent>,
    adapters: &mut AccessKitAdapters,
    handlers: &mut WinitActionRequestHandlers,
) {
    if let Some(mut old) = adapters.remove(&entity) {
        handlers.pending_lifecycle.extend(retire_adapter_state(
            entity,
            old.generation,
            &old.runtime,
            &old.window_state,
            &mut old.providers,
            RuntimeRetirement::WakeOwner,
        ));
    }
    handlers.handlers.remove(&entity);
    window_state.set_platform_active(false);
    handlers.next_adapter_generation = handlers.next_adapter_generation.wrapping_add(1);
    let generation = AccessibilityAdapterGeneration(handlers.next_adapter_generation);
    let runtime = Arc::new(Mutex::new(WinitAccessibilityRuntime {
        adapter_generation: generation,
        wake_callback: Some(Arc::new(move || {
            event_loop_proxy.send_event(WinitUserEvent::WakeUp).is_ok()
        })),
        ..Default::default()
    }));
    let state = AccessKitState::new(
        name,
        entity,
        accessibility_requested,
        window_state.clone(),
        Arc::clone(&runtime),
    );
    let activation_handler = WinitActivationHandler::new(Arc::clone(&state));

    let action_handler = WinitActionHandler::new(generation, Arc::clone(&runtime));
    let deactivation_handler = WinitDeactivationHandler {
        window_state: window_state.clone(),
        runtime: Arc::clone(&runtime),
    };

    let adapter = Adapter::with_direct_handlers(
        event_loop,
        winit_window,
        activation_handler,
        action_handler,
        deactivation_handler,
    );

    adapters.insert(
        entity,
        AccessKitAdapter {
            adapter,
            generation,
            runtime: Arc::clone(&runtime),
            window_state,
            providers: EntityHashMap::default(),
            next_attachment_generation: 0,
            last_root_activation: None,
            last_root: None,
            focus_conflict_active: false,
            invalid_grafts: EntityHashSet::default(),
        },
    );
    handlers.handlers.insert(entity, runtime);
}

fn window_closed(
    mut handlers: ResMut<WinitActionRequestHandlers>,
    mut window_closed_reader: MessageReader<WindowClosed>,
    mut lifecycle: MessageWriter<AccessibilitySubtreeLifecycle>,
    mut follow_up: ResMut<AccessibilityConsumerFollowUp>,
    _non_send_marker: NonSendMarker,
) {
    let mut closed_any = false;
    ACCESS_KIT_ADAPTERS.with_borrow_mut(|adapters| {
        for WindowClosed { window, .. } in window_closed_reader.read() {
            closed_any = true;
            if let Some(mut adapter) = adapters.remove(window) {
                for message in retire_adapter_state(
                    *window,
                    adapter.generation,
                    &adapter.runtime,
                    &adapter.window_state,
                    &mut adapter.providers,
                    RuntimeRetirement::Disconnect,
                ) {
                    lifecycle.write(message);
                }
            } else if let Some(runtime) = handlers.handlers.get(window) {
                retire_runtime(runtime, None, RuntimeRetirement::Disconnect);
            }
            handlers.handlers.remove(window);
        }
    });
    // `WindowClosed` is produced in `Last`. Retire the adapter in that same
    // schedule and request one update so ordinary `Update` lifecycle consumers
    // observe any deactivation messages without relying on a later OS event.
    follow_up.0 |= closed_any;
}

fn poll_receivers(
    handlers: Res<WinitActionRequestHandlers>,
    mut actions: MessageWriter<ActionRequestWrapper>,
    mut subtree_actions: MessageWriter<AccessibilitySubtreeActionRequest>,
    mut follow_up: ResMut<AccessibilityConsumerFollowUp>,
) {
    let mut forwarded_action = false;
    for (window, runtime) in &handlers.handlers {
        let mut runtime = runtime.lock().unwrap();
        while let Some(queued) = runtime.actions.pop_front() {
            if queued.request.target_tree == TreeId::ROOT {
                if queued_action_is_current(&runtime, &queued) {
                    actions.write(ActionRequestWrapper(queued.request));
                    forwarded_action = true;
                }
                continue;
            }
            if let Some(action) = validate_subtree_action(*window, &runtime, queued) {
                subtree_actions.write(action);
                forwarded_action = true;
            }
        }
        runtime.acknowledge_owner_pass();
    }
    // `Actions` is in PostUpdate, after ordinary application `Update`
    // consumers. Guarantee one more update so consumers that do not opt into
    // `AccessibilitySystems::Actions` cannot be stranded in a low-power loop.
    if forwarded_action {
        follow_up.0 = true;
    }
}

fn flush_pending_lifecycle(
    mut handlers: ResMut<WinitActionRequestHandlers>,
    mut lifecycle: MessageWriter<AccessibilitySubtreeLifecycle>,
    mut follow_up: ResMut<AccessibilityConsumerFollowUp>,
) {
    for message in handlers.pending_lifecycle.drain(..) {
        lifecycle.write(message);
        follow_up.0 = true;
    }
}

fn validate_subtree_action(
    window: Entity,
    runtime: &WinitAccessibilityRuntime,
    queued: QueuedAction,
) -> Option<AccessibilitySubtreeActionRequest> {
    if !queued_action_is_current(runtime, &queued) {
        return None;
    }
    let route = queued.route?;
    if !runtime
        .routes
        .get(&queued.request.target_tree)
        .is_some_and(|current| current.same_attachment_snapshot(&route))
    {
        return None;
    }
    Some(AccessibilitySubtreeActionRequest {
        window,
        provider: route.provider,
        adapter_generation: queued.adapter_generation,
        activation_generation: queued.activation_generation,
        attachment_generation: route.attachment_generation,
        snapshot_sequence: route.snapshot_sequence,
        committed_transform: route.committed_transform,
        request: queued.request,
    })
}

fn queued_action_is_current(runtime: &WinitAccessibilityRuntime, queued: &QueuedAction) -> bool {
    runtime.active
        && queued.adapter_generation == runtime.adapter_generation
        && queued.activation_generation
            == AccessibilityActivationGeneration(runtime.activation_generation)
}

fn should_update_accessibility_nodes(
    accessibility_requested: Res<AccessibilityRequested>,
    manage_accessibility_updates: Res<ManageAccessibilityUpdates>,
) -> bool {
    accessibility_requested.get() && manage_accessibility_updates.get()
}

fn begin_accessibility_owner_pass(handlers: Res<WinitActionRequestHandlers>) {
    for runtime in handlers.handlers.values() {
        runtime.lock().unwrap().begin_owner_pass();
    }
}

fn update_accessibility_nodes(
    focus: Option<Res<InputFocus>>,
    windows: Query<(Entity, &Window, Has<PrimaryWindow>)>,
    nodes: Query<(
        Entity,
        &AccessibilityNode,
        Option<&Children>,
        Option<&ChildOf>,
    )>,
    node_entities: Query<Entity, With<AccessibilityNode>>,
    providers: Query<(
        Entity,
        Ref<AccessibilitySubtreeProvider>,
        Option<Ref<AccessibilitySubtreeSnapshot>>,
        Option<Ref<AccessibilitySubtreeOrder>>,
        Option<Ref<AccessibilitySubtreeTransform>>,
        Option<Ref<AccessibilitySubtreeParentFocus>>,
    )>,
    mut boundary: ResMut<AccessibilityCompositionBoundary>,
    mut handlers: ResMut<WinitActionRequestHandlers>,
    mut follow_up: ResMut<AccessibilityConsumerFollowUp>,
    _non_send_marker: NonSendMarker,
) {
    let provider_inputs: Vec<_> = providers
        .iter()
        .map(
            |(entity, provider, snapshot, order, transform, parent_focus)| ProviderInput {
                entity,
                provider: *provider,
                provider_tick: provider.last_changed().get(),
                provider_changed: provider.is_changed(),
                snapshot_tick: snapshot.as_ref().map(|value| value.last_changed().get()),
                snapshot: snapshot.map(|value| (*value).clone()),
                order_tick: order.as_ref().map(|value| value.last_changed().get()),
                order: order.as_deref().copied().unwrap_or_default(),
                transform_tick: transform.as_ref().map(|value| value.last_changed().get()),
                transform: transform.as_deref().copied(),
                parent_focus_tick: parent_focus
                    .as_ref()
                    .map(|value| value.last_changed().get()),
                parent_focused: parent_focus.is_some(),
            },
        )
        .collect();
    boundary.0 = provider_inputs
        .iter()
        .map(ProviderInput::certificate)
        .collect();
    boundary.0.sort_by_key(|entry| entry.entity.to_bits());

    ACCESS_KIT_ADAPTERS.with_borrow_mut(|adapters| {
        for (window_entity, window, is_primary) in &windows {
            let Some(adapter) = adapters.get_mut(&window_entity) else {
                continue;
            };
            let inputs: Vec<_> = provider_inputs
                .iter()
                .filter(|input| input.provider.window() == window_entity)
                .collect();
            follow_up.0 |= compose_window(
                adapter,
                window_entity,
                window,
                is_primary,
                focus.as_deref(),
                nodes,
                node_entities,
                &inputs,
                &mut handlers.pending_lifecycle,
            );
        }
    });
}

struct ProviderInput {
    entity: Entity,
    provider: AccessibilitySubtreeProvider,
    provider_tick: u32,
    provider_changed: bool,
    snapshot: Option<AccessibilitySubtreeSnapshot>,
    snapshot_tick: Option<u32>,
    order: AccessibilitySubtreeOrder,
    order_tick: Option<u32>,
    transform: Option<AccessibilitySubtreeTransform>,
    transform_tick: Option<u32>,
    parent_focused: bool,
    parent_focus_tick: Option<u32>,
}

impl ProviderInput {
    fn certificate(&self) -> ProviderCertificate {
        ProviderCertificate {
            entity: self.entity,
            provider: self.provider,
            provider_tick: self.provider_tick,
            snapshot_sequence: self
                .snapshot
                .as_ref()
                .map(AccessibilitySubtreeSnapshot::sequence),
            snapshot_tick: self.snapshot_tick,
            order: self.order,
            order_tick: self.order_tick,
            transform_bits: self
                .transform
                .map(|transform| transform.0.as_coeffs().map(f64::to_bits)),
            transform_tick: self.transform_tick,
            parent_focused: self.parent_focused,
            parent_focus_tick: self.parent_focus_tick,
        }
    }
}

fn request_accessibility_update_after_boundary(
    providers: Query<(
        Entity,
        Ref<AccessibilitySubtreeProvider>,
        Option<Ref<AccessibilitySubtreeSnapshot>>,
        Option<Ref<AccessibilitySubtreeOrder>>,
        Option<Ref<AccessibilitySubtreeTransform>>,
        Option<Ref<AccessibilitySubtreeParentFocus>>,
    )>,
    boundary: Res<AccessibilityCompositionBoundary>,
    accessibility_requested: Res<AccessibilityRequested>,
    manage_accessibility_updates: Res<ManageAccessibilityUpdates>,
    mut follow_up: ResMut<AccessibilityConsumerFollowUp>,
    mut redraw: MessageWriter<RequestRedraw>,
) {
    let consumer_follow_up = core::mem::take(&mut follow_up.0);
    if !accessibility_requested.get() || !manage_accessibility_updates.get() {
        if consumer_follow_up {
            redraw.write(RequestRedraw);
        }
        return;
    }
    let mut current: Vec<_> = providers
        .iter()
        .map(
            |(entity, provider, snapshot, order, transform, parent_focus)| ProviderCertificate {
                entity,
                provider: *provider,
                provider_tick: provider.last_changed().get(),
                snapshot_sequence: snapshot.as_ref().map(|snapshot| snapshot.sequence()),
                snapshot_tick: snapshot.as_ref().map(|value| value.last_changed().get()),
                order: order.as_deref().copied().unwrap_or_default(),
                order_tick: order.as_ref().map(|value| value.last_changed().get()),
                transform_bits: transform
                    .as_ref()
                    .map(|transform| transform.0.as_coeffs().map(f64::to_bits)),
                transform_tick: transform.as_ref().map(|value| value.last_changed().get()),
                parent_focused: parent_focus.is_some(),
                parent_focus_tick: parent_focus
                    .as_ref()
                    .map(|value| value.last_changed().get()),
            },
        )
        .collect();
    current.sort_by_key(|entry| entry.entity.to_bits());
    if consumer_follow_up || current != boundary.0 {
        redraw.write(RequestRedraw);
    }
}

#[expect(
    clippy::too_many_arguments,
    reason = "This is the platform ownership transaction for one window."
)]
fn compose_window(
    adapter: &mut AccessKitAdapter,
    window_entity: Entity,
    window: &Window,
    is_primary: bool,
    focus: Option<&InputFocus>,
    nodes: Query<(
        Entity,
        &AccessibilityNode,
        Option<&Children>,
        Option<&ChildOf>,
    )>,
    node_entities: Query<Entity, With<AccessibilityNode>>,
    inputs: &[&ProviderInput],
    lifecycle: &mut Vec<AccessibilitySubtreeLifecycle>,
) -> bool {
    let mut consumer_follow_up = false;
    prune_invalid_grafts(&mut adapter.invalid_grafts, inputs);
    let inputs: Vec<_> = inputs
        .iter()
        .copied()
        .filter(|input| {
            let available = graft_node_id_is_available(
                input.entity,
                window_entity,
                is_primary && node_entities.contains(input.entity),
            );
            if !available && adapter.invalid_grafts.insert(input.entity) {
                warn!(
                    provider = ?input.entity,
                    window = ?window_entity,
                    "accessibility subtree provider graft NodeId collides with the native root tree"
                );
            }
            if available {
                adapter.invalid_grafts.remove(&input.entity);
            }
            available
        })
        .collect();
    let tree_id_counts = count_tree_ids(&inputs);
    let current_entities: EntityHashSet = inputs.iter().map(|input| input.entity).collect();
    let removed: Vec<_> = adapter
        .providers
        .keys()
        .filter(|entity| !current_entities.contains(*entity))
        .copied()
        .collect();
    for provider in removed {
        if let Some(old) = adapter.providers.remove(&provider) {
            adapter.runtime.lock().unwrap().routes.remove(&old.tree_id);
            if let Some(activation_generation) = old.notified_activation {
                lifecycle.push(AccessibilitySubtreeLifecycle::Deactivated(lifecycle_token(
                    window_entity,
                    provider,
                    old.tree_id,
                    adapter.generation,
                    activation_generation,
                    old.attachment_generation,
                )));
                consumer_follow_up = true;
            }
        }
    }

    for input in &inputs {
        let changed_attachment = adapter
            .providers
            .get(&input.entity)
            .is_none_or(|old| old.tree_id != input.provider.tree_id())
            || input.provider_changed;
        if changed_attachment {
            if let Some(old) = adapter.providers.remove(&input.entity) {
                adapter.runtime.lock().unwrap().routes.remove(&old.tree_id);
                if let Some(activation_generation) = old.notified_activation {
                    lifecycle.push(AccessibilitySubtreeLifecycle::Deactivated(lifecycle_token(
                        window_entity,
                        input.entity,
                        old.tree_id,
                        adapter.generation,
                        activation_generation,
                        old.attachment_generation,
                    )));
                    consumer_follow_up = true;
                }
            }
            adapter.next_attachment_generation = adapter.next_attachment_generation.wrapping_add(1);
            adapter.providers.insert(
                input.entity,
                AttachedProvider {
                    tree_id: input.provider.tree_id(),
                    attachment_generation: AccessibilityAttachmentGeneration(
                        adapter.next_attachment_generation,
                    ),
                    snapshot_sequence: None,
                    snapshot: None,
                    notified_activation: None,
                    published_activation: None,
                    published_sequence: None,
                    detached: false,
                    duplicate_invalid: false,
                },
            );
        }
    }

    let (runtime_active, activation_generation) = {
        let mut runtime = adapter.runtime.lock().unwrap();
        let pending_deactivations: Vec<_> =
            runtime.pending_deactivation_generations.drain(..).collect();
        for input in &inputs {
            let attached = adapter.providers.get_mut(&input.entity).unwrap();
            for emission in compose_provider_lifecycle(
                &mut adapter.next_attachment_generation,
                &mut runtime,
                attached,
                input.snapshot.as_ref(),
                tree_id_counts.get(&input.provider.tree_id()) == Some(&1),
                &pending_deactivations,
            ) {
                match emission {
                    ProviderLifecycleEmission::Activated {
                        activation_generation,
                        attachment_generation,
                    } => {
                        lifecycle.push(AccessibilitySubtreeLifecycle::Activated(lifecycle_token(
                            window_entity,
                            input.entity,
                            attached.tree_id,
                            adapter.generation,
                            activation_generation,
                            attachment_generation,
                        )));
                    }
                    ProviderLifecycleEmission::Deactivated {
                        activation_generation,
                        attachment_generation,
                    } => {
                        lifecycle.push(AccessibilitySubtreeLifecycle::Deactivated(
                            lifecycle_token(
                                window_entity,
                                input.entity,
                                attached.tree_id,
                                adapter.generation,
                                activation_generation,
                                attachment_generation,
                            ),
                        ));
                    }
                }
                consumer_follow_up = true;
            }
        }
        (
            runtime.active,
            AccessibilityActivationGeneration(runtime.activation_generation),
        )
    };
    if !runtime_active {
        return consumer_follow_up;
    }

    let mut published: Vec<_> = inputs
        .iter()
        .filter_map(|input| {
            if tree_id_counts.get(&input.provider.tree_id()) != Some(&1) {
                return None;
            }
            let attached = adapter.providers.get(&input.entity)?;
            let snapshot_sequence = attached.snapshot_sequence?;
            attached.snapshot.as_ref()?;
            Some((
                input.entity,
                input.order,
                input.transform,
                input.parent_focused,
                attached.tree_id,
                attached.attachment_generation,
                snapshot_sequence,
                attached.published_activation,
                attached.published_sequence,
            ))
        })
        .collect();
    published.sort_by_key(|(entity, order, ..)| (order.0, entity.to_bits()));

    let to_publish: Vec<_> = published
        .iter()
        .filter(
            |(_, _, _, _, _, _, snapshot_sequence, published_activation, published_sequence)| {
                *published_activation != Some(activation_generation)
                    || *published_sequence != Some(*snapshot_sequence)
            },
        )
        .map(
            |(entity, _, transform, _, tree_id, attachment_generation, snapshot_sequence, _, _)| {
                (
                    *entity,
                    *tree_id,
                    *attachment_generation,
                    *snapshot_sequence,
                    *transform,
                    materialize_snapshot(
                        adapter.providers[entity]
                            .snapshot
                            .as_ref()
                            .expect("published providers have retained snapshots"),
                        *tree_id,
                    ),
                )
            },
        )
        .collect();

    let focused_claims: Vec<_> = published
        .iter()
        .filter(|(_, _, _, focused, ..)| *focused)
        .map(|(entity, ..)| *entity)
        .collect();
    let parent_focus = resolve_parent_focus(&focused_claims);
    let focus_conflict = focused_claims.len() > 1;
    if focus_conflict && !adapter.focus_conflict_active {
        warn!(
            window = ?window_entity,
            providers = ?focused_claims,
            "multiple accessibility subtree providers claimed parent focus; focus fails closed"
        );
    }
    adapter.focus_conflict_active = focus_conflict;
    let grafts: Vec<_> = published
        .iter()
        .map(|(entity, _, transform, _, tree_id, ..)| {
            let mut node = Node::new(Role::GenericContainer);
            node.set_tree_id(*tree_id);
            if let Some(transform) = transform {
                node.set_transform(transform.0);
            }
            (*entity, node)
        })
        .collect();
    let desired_root = update_adapter(
        nodes,
        node_entities,
        window,
        window_entity,
        is_primary,
        focus,
        &grafts,
        parent_focus,
    );
    let needs_root = adapter.last_root_activation != Some(activation_generation)
        || adapter.last_root.as_ref() != Some(&desired_root)
        || !to_publish.is_empty();
    if !needs_root {
        return consumer_follow_up;
    }
    let unfocused_root = {
        let mut root = desired_root.clone();
        root.focus = NodeId(window_entity.to_bits());
        root
    };
    let safe_focus = if adapter.last_root_activation == Some(activation_generation) {
        adapter
            .last_root
            .as_ref()
            .map(|root| root.focus)
            .filter(|previous_focus| {
                let present_in_root = desired_root
                    .nodes
                    .iter()
                    .any(|(node_id, _)| node_id == previous_focus);
                if !present_in_root {
                    return false;
                }
                let previous_provider = published
                    .iter()
                    .find(|(entity, ..)| NodeId(entity.to_bits()) == *previous_focus);
                previous_provider.is_none_or(
                    |(_, _, _, _, _, _, _, published_activation, published_sequence)| {
                        *published_activation == Some(activation_generation)
                            && published_sequence.is_some()
                    },
                )
            })
            .unwrap_or(unfocused_root.focus)
    } else {
        unfocused_root.focus
    };
    let safe_root = {
        let mut root = desired_root.clone();
        root.focus = safe_focus;
        root
    };
    let publication_metadata: Vec<_> = to_publish
        .iter()
        .map(
            |(entity, tree_id, attachment_generation, snapshot_sequence, transform, _)| {
                (
                    *entity,
                    *tree_id,
                    *attachment_generation,
                    *snapshot_sequence,
                    *transform,
                )
            },
        )
        .collect();
    let committed_routes: Vec<_> = published
        .iter()
        .map(
            |(entity, _, transform, _, tree_id, attachment_generation, snapshot_sequence, ..)| {
                (
                    *tree_id,
                    RouteToken {
                        provider: *entity,
                        attachment_generation: *attachment_generation,
                        snapshot_sequence: *snapshot_sequence,
                        committed_transform: *transform,
                    },
                )
            },
        )
        .collect();
    let changing_trees: HashSet<_> = publication_metadata
        .iter()
        .map(|(_, tree_id, ..)| *tree_id)
        .collect();
    let stable_root_routes: Vec<_> = committed_routes
        .iter()
        .filter(|(tree_id, _)| !changing_trees.contains(tree_id))
        .copied()
        .collect();
    let mut updates = root_first_updates(
        safe_root.clone(),
        to_publish
            .into_iter()
            .map(|(_, _, _, _, _, snapshot)| snapshot),
    );
    suspend_routes_for_transaction(
        &mut adapter.runtime.lock().unwrap(),
        committed_routes.iter().map(|(tree_id, _)| *tree_id),
    );
    let mut root_sent = false;
    let mut root_generation_valid = false;
    adapter.adapter.update_if_active(|| {
        root_sent = true;
        root_generation_valid = runtime_matches_generation(
            &adapter.runtime.lock().unwrap(),
            adapter.generation,
            activation_generation,
        );
        if root_generation_valid {
            updates.next().unwrap()
        } else {
            unfocused_root.clone()
        }
    });
    if !root_sent
        || !root_generation_valid
        || !commit_routes_if_current(
            &mut adapter.runtime.lock().unwrap(),
            adapter.generation,
            activation_generation,
            &stable_root_routes,
        )
    {
        return consumer_follow_up;
    }
    let mut all_subtrees_committed = true;
    for ((entity, tree_id, attachment_generation, snapshot_sequence, transform), snapshot) in
        publication_metadata.into_iter().zip(updates)
    {
        let route = RouteToken {
            provider: entity,
            attachment_generation,
            snapshot_sequence,
            committed_transform: transform,
        };
        let mut subtree_sent = false;
        let mut subtree_generation_valid = false;
        suspend_routes_for_transaction(
            &mut adapter.runtime.lock().unwrap(),
            core::iter::once(tree_id),
        );
        adapter.adapter.update_if_active(|| {
            subtree_sent = true;
            let (update, valid) = select_generation_safe_publication(
                &adapter.runtime.lock().unwrap(),
                adapter.generation,
                activation_generation,
                snapshot,
                || unfocused_root.clone(),
            );
            subtree_generation_valid = valid;
            update
        });
        if !subtree_sent
            || !subtree_generation_valid
            || !commit_route_if_current(
                &mut adapter.runtime.lock().unwrap(),
                adapter.generation,
                activation_generation,
                tree_id,
                route,
            )
        {
            all_subtrees_committed = false;
            break;
        }
        let current = adapter.providers.get_mut(&entity).unwrap();
        current.published_activation = Some(activation_generation);
        current.published_sequence = Some(snapshot_sequence);
    }
    if !all_subtrees_committed {
        adapter.last_root_activation = None;
        adapter.last_root = Some(safe_root);
        return consumer_follow_up;
    }

    if desired_root != safe_root {
        suspend_routes_for_transaction(
            &mut adapter.runtime.lock().unwrap(),
            committed_routes.iter().map(|(tree_id, _)| *tree_id),
        );
        let mut final_sent = false;
        let mut final_generation_valid = false;
        let final_root = desired_root.clone();
        adapter.adapter.update_if_active(|| {
            final_sent = true;
            final_generation_valid = runtime_matches_generation(
                &adapter.runtime.lock().unwrap(),
                adapter.generation,
                activation_generation,
            );
            if final_generation_valid {
                final_root
            } else {
                unfocused_root.clone()
            }
        });
        if !final_sent
            || !final_generation_valid
            || !commit_routes_if_current(
                &mut adapter.runtime.lock().unwrap(),
                adapter.generation,
                activation_generation,
                &committed_routes,
            )
        {
            adapter.last_root_activation = None;
            adapter.last_root = Some(safe_root);
            return consumer_follow_up;
        }
    }
    adapter.last_root_activation = Some(activation_generation);
    adapter.last_root = Some(desired_root);
    consumer_follow_up
}

fn count_tree_ids(inputs: &[&ProviderInput]) -> HashMap<TreeId, usize> {
    let mut counts = HashMap::default();
    for input in inputs {
        *counts.entry(input.provider.tree_id()).or_default() += 1;
    }
    counts
}

fn materialize_snapshot(snapshot: &AccessibilitySubtreeSnapshot, tree_id: TreeId) -> TreeUpdate {
    #[cfg(test)]
    SNAPSHOT_MATERIALIZATIONS.set(SNAPSHOT_MATERIALIZATIONS.get() + 1);
    snapshot.tree_update(tree_id)
}

fn prune_invalid_grafts(invalid_grafts: &mut EntityHashSet, inputs: &[&ProviderInput]) {
    invalid_grafts.retain(|entity| inputs.iter().any(|input| input.entity == *entity));
}

fn graft_node_id_is_available(
    provider: Entity,
    window: Entity,
    provider_is_primary_accessibility_node: bool,
) -> bool {
    provider != window && !provider_is_primary_accessibility_node
}

fn runtime_matches_generation(
    runtime: &WinitAccessibilityRuntime,
    adapter_generation: AccessibilityAdapterGeneration,
    activation_generation: AccessibilityActivationGeneration,
) -> bool {
    runtime.active
        && runtime.adapter_generation == adapter_generation
        && AccessibilityActivationGeneration(runtime.activation_generation) == activation_generation
}

fn commit_route_if_current(
    runtime: &mut WinitAccessibilityRuntime,
    adapter_generation: AccessibilityAdapterGeneration,
    activation_generation: AccessibilityActivationGeneration,
    tree_id: TreeId,
    route: RouteToken,
) -> bool {
    if !runtime_matches_generation(runtime, adapter_generation, activation_generation) {
        return false;
    }
    runtime.routes.insert(tree_id, route);
    true
}

fn commit_routes_if_current(
    runtime: &mut WinitAccessibilityRuntime,
    adapter_generation: AccessibilityAdapterGeneration,
    activation_generation: AccessibilityActivationGeneration,
    committed_routes: &[(TreeId, RouteToken)],
) -> bool {
    if !runtime_matches_generation(runtime, adapter_generation, activation_generation) {
        return false;
    }
    for (tree_id, route) in committed_routes {
        runtime.routes.insert(*tree_id, *route);
    }
    true
}

fn suspend_routes_for_transaction(
    runtime: &mut WinitAccessibilityRuntime,
    tree_ids: impl IntoIterator<Item = TreeId>,
) {
    for tree_id in tree_ids {
        runtime.routes.remove(&tree_id);
    }
}

fn select_generation_safe_publication(
    runtime: &WinitAccessibilityRuntime,
    adapter_generation: AccessibilityAdapterGeneration,
    activation_generation: AccessibilityActivationGeneration,
    subtree: TreeUpdate,
    fallback_root: impl FnOnce() -> TreeUpdate,
) -> (TreeUpdate, bool) {
    if runtime_matches_generation(runtime, adapter_generation, activation_generation) {
        (subtree, true)
    } else {
        (fallback_root(), false)
    }
}

fn compose_provider_lifecycle(
    next_attachment_generation: &mut u64,
    runtime: &mut WinitAccessibilityRuntime,
    attached: &mut AttachedProvider,
    snapshot: Option<&AccessibilitySubtreeSnapshot>,
    tree_id_is_unique: bool,
    pending_deactivations: &[AccessibilityActivationGeneration],
) -> Vec<ProviderLifecycleEmission> {
    let mut emissions = Vec::new();
    for activation_generation in pending_deactivations {
        if attached.notified_activation == Some(*activation_generation) {
            attached.notified_activation = None;
            emissions.push(ProviderLifecycleEmission::Deactivated {
                activation_generation: *activation_generation,
                attachment_generation: attached.attachment_generation,
            });
        }
    }
    match snapshot {
        Some(snapshot)
            if attached
                .snapshot_sequence
                .is_none_or(|sequence| snapshot.sequence() > sequence) =>
        {
            accept_provider_snapshot(next_attachment_generation, attached, snapshot);
        }
        None if attached.snapshot.is_some() => {
            runtime.routes.remove(&attached.tree_id);
            if let Some((activation_generation, attachment_generation)) =
                detach_provider_snapshot(attached)
            {
                emissions.push(ProviderLifecycleEmission::Deactivated {
                    activation_generation,
                    attachment_generation,
                });
            }
        }
        _ => {}
    }

    if !runtime.active {
        if let Some(activation_generation) = attached.notified_activation.take() {
            emissions.push(ProviderLifecycleEmission::Deactivated {
                activation_generation,
                attachment_generation: attached.attachment_generation,
            });
        }
        return emissions;
    }

    if attached.detached {
        return emissions;
    }

    if !tree_id_is_unique {
        let (attachment_generation, activation_generation) =
            invalidate_duplicate_attachment(next_attachment_generation, runtime, attached);
        if let Some(activation_generation) = activation_generation {
            emissions.push(ProviderLifecycleEmission::Deactivated {
                activation_generation,
                attachment_generation,
            });
        }
        return emissions;
    }

    attached.duplicate_invalid = false;
    let activation_generation = AccessibilityActivationGeneration(runtime.activation_generation);
    if attached.notified_activation != Some(activation_generation) {
        attached.notified_activation = Some(activation_generation);
        emissions.push(ProviderLifecycleEmission::Activated {
            activation_generation,
            attachment_generation: attached.attachment_generation,
        });
    }
    emissions
}

fn accept_provider_snapshot(
    next_attachment_generation: &mut u64,
    attached: &mut AttachedProvider,
    snapshot: &AccessibilitySubtreeSnapshot,
) {
    if attached.detached {
        *next_attachment_generation = (*next_attachment_generation).wrapping_add(1);
        attached.attachment_generation =
            AccessibilityAttachmentGeneration(*next_attachment_generation);
        attached.detached = false;
    }
    attached.snapshot_sequence = Some(snapshot.sequence());
    attached.snapshot = Some(snapshot.clone());
    attached.published_activation = None;
    attached.published_sequence = None;
}

fn detach_provider_snapshot(
    attached: &mut AttachedProvider,
) -> Option<(
    AccessibilityActivationGeneration,
    AccessibilityAttachmentGeneration,
)> {
    let old_attachment = attached.attachment_generation;
    attached.snapshot_sequence = None;
    attached.snapshot = None;
    attached.published_activation = None;
    attached.published_sequence = None;
    attached.detached = true;
    attached
        .notified_activation
        .take()
        .map(|activation| (activation, old_attachment))
}

fn invalidate_duplicate_tree_id(
    runtime: &mut WinitAccessibilityRuntime,
    attached: &mut AttachedProvider,
) -> Option<AccessibilityActivationGeneration> {
    runtime.routes.remove(&attached.tree_id);
    attached.published_activation = None;
    attached.published_sequence = None;
    attached.notified_activation.take()
}

fn invalidate_duplicate_attachment(
    next_attachment_generation: &mut u64,
    runtime: &mut WinitAccessibilityRuntime,
    attached: &mut AttachedProvider,
) -> (
    AccessibilityAttachmentGeneration,
    Option<AccessibilityActivationGeneration>,
) {
    let previous_attachment_generation = attached.attachment_generation;
    if !attached.duplicate_invalid {
        *next_attachment_generation = (*next_attachment_generation).wrapping_add(1);
        attached.attachment_generation =
            AccessibilityAttachmentGeneration(*next_attachment_generation);
        attached.duplicate_invalid = true;
        warn!(
            tree_id = ?attached.tree_id,
            "duplicate accessibility subtree TreeId invalidated all conflicting attachments"
        );
    }
    (
        previous_attachment_generation,
        invalidate_duplicate_tree_id(runtime, attached),
    )
}

fn root_first_updates(
    root: TreeUpdate,
    subtrees: impl IntoIterator<Item = TreeUpdate>,
) -> impl Iterator<Item = TreeUpdate> {
    core::iter::once(root).chain(subtrees)
}

fn lifecycle_token(
    window: Entity,
    provider: Entity,
    tree_id: TreeId,
    adapter_generation: AccessibilityAdapterGeneration,
    activation_generation: AccessibilityActivationGeneration,
    attachment_generation: AccessibilityAttachmentGeneration,
) -> AccessibilitySubtreeLifecycleToken {
    AccessibilitySubtreeLifecycleToken {
        window,
        provider,
        tree_id,
        adapter_generation,
        activation_generation,
        attachment_generation,
    }
}

fn take_provider_deactivations(
    window: Entity,
    adapter_generation: AccessibilityAdapterGeneration,
    providers: &mut EntityHashMap<AttachedProvider>,
) -> Vec<AccessibilitySubtreeLifecycle> {
    providers
        .iter_mut()
        .filter_map(|(provider, attached)| {
            attached
                .notified_activation
                .take()
                .map(|activation_generation| {
                    AccessibilitySubtreeLifecycle::Deactivated(lifecycle_token(
                        window,
                        *provider,
                        attached.tree_id,
                        adapter_generation,
                        activation_generation,
                        attached.attachment_generation,
                    ))
                })
        })
        .collect()
}

fn retire_adapter_state(
    window: Entity,
    adapter_generation: AccessibilityAdapterGeneration,
    runtime: &Arc<Mutex<WinitAccessibilityRuntime>>,
    window_state: &WindowAccessibilityState,
    providers: &mut EntityHashMap<AttachedProvider>,
    retirement: RuntimeRetirement,
) -> Vec<AccessibilitySubtreeLifecycle> {
    let messages = take_provider_deactivations(window, adapter_generation, providers);
    retire_runtime(runtime, Some(window_state), retirement);
    messages
}

enum RuntimeRetirement {
    WakeOwner,
    Disconnect,
}

fn retire_runtime(
    runtime: &Arc<Mutex<WinitAccessibilityRuntime>>,
    window_state: Option<&WindowAccessibilityState>,
    retirement: RuntimeRetirement,
) {
    let mut runtime = runtime.lock().unwrap();
    runtime.active = false;
    runtime.routes.clear();
    runtime.actions.clear();
    runtime.pending_deactivation_generations.clear();
    if let Some(window_state) = window_state {
        window_state.set_platform_active(false);
    }
    match retirement {
        RuntimeRetirement::WakeOwner => runtime.produce_callback(),
        RuntimeRetirement::Disconnect => {
            runtime.owner_observed_epoch = None;
            runtime.consumed_callback_epoch = runtime.produced_callback_epoch;
            runtime.wake_sent_epoch = runtime.produced_callback_epoch;
            // Adapter destruction may invoke the deactivation handler. Sever
            // the proxy first so the explicit RequestRedraw, or terminal exit,
            // remains the sole owner of any follow-up.
            runtime.wake_callback = None;
        }
    }
}

/// Retires every native accessibility object at the terminal runner boundary.
///
/// Exit has no later application update in which lifecycle messages could be
/// observed, so this atomically invalidates all routes and platform state,
/// discards queued lifecycle edges, and drops adapters without sending a wake.
pub(crate) fn shutdown_accessibility(world: &mut World) {
    ACCESS_KIT_ADAPTERS.with_borrow_mut(|adapters| {
        for adapter in adapters.values() {
            retire_runtime(
                &adapter.runtime,
                Some(&adapter.window_state),
                RuntimeRetirement::Disconnect,
            );
        }
        adapters.clear();
    });
    if let Some(mut handlers) = world.get_resource_mut::<WinitActionRequestHandlers>() {
        for runtime in handlers.handlers.values() {
            retire_runtime(runtime, None, RuntimeRetirement::Disconnect);
        }
        handlers.handlers.clear();
        handlers.pending_lifecycle.clear();
    }
}

fn resolve_parent_focus(focused_claims: &[Entity]) -> Option<Entity> {
    let [only] = focused_claims else {
        return None;
    };
    Some(*only)
}

fn update_adapter(
    nodes: Query<(
        Entity,
        &AccessibilityNode,
        Option<&Children>,
        Option<&ChildOf>,
    )>,
    node_entities: Query<Entity, With<AccessibilityNode>>,
    window: &Window,
    window_entity: Entity,
    include_ecs_nodes: bool,
    focus: Option<&InputFocus>,
    grafts: &[(Entity, Node)],
    subtree_focus: Option<Entity>,
) -> TreeUpdate {
    let mut to_update = vec![];
    let mut window_children = vec![];
    if include_ecs_nodes {
        for (entity, node, children, child_of) in &nodes {
            let mut node = (**node).clone();
            queue_node_for_update(entity, child_of, &node_entities, &mut window_children);
            add_children_nodes(children, &node_entities, &mut node);
            let node_id = NodeId(entity.to_bits());
            to_update.push((node_id, node));
        }
    }
    for (entity, node) in grafts {
        window_children.push(NodeId(entity.to_bits()));
        to_update.push((NodeId(entity.to_bits()), node.clone()));
    }
    let mut window_node = Node::new(Role::Window);
    if window.focused {
        let title = window.title.clone();
        window_node.set_label(title.into_boxed_str());
    }
    window_node.set_children(window_children);
    let node_id = NodeId(window_entity.to_bits());
    let window_update = (node_id, window_node);
    to_update.insert(0, window_update);
    let root_focus = subtree_focus
        .or_else(|| {
            include_ecs_nodes
                .then(|| focus.and_then(InputFocus::get))
                .flatten()
        })
        .filter(|focused| {
            grafts.iter().any(|(entity, _)| entity == focused)
                || (include_ecs_nodes && node_entities.contains(*focused))
        })
        .unwrap_or(window_entity);
    TreeUpdate {
        nodes: to_update,
        tree: None,
        tree_id: TreeId::ROOT,
        focus: NodeId(root_focus.to_bits()),
    }
}

#[inline]
fn queue_node_for_update(
    node_entity: Entity,
    child_of: Option<&ChildOf>,
    node_entities: &Query<Entity, With<AccessibilityNode>>,
    window_children: &mut Vec<NodeId>,
) {
    let should_push = if let Some(child_of) = child_of {
        !node_entities.contains(child_of.parent())
    } else {
        true
    };
    if should_push {
        window_children.push(NodeId(node_entity.to_bits()));
    }
}

#[inline]
fn add_children_nodes(
    children: Option<&Children>,
    node_entities: &Query<Entity, With<AccessibilityNode>>,
    node: &mut Node,
) {
    let Some(children) = children else {
        return;
    };
    for child in children {
        if node_entities.contains(*child) {
            node.push_child(NodeId(child.to_bits()));
        }
    }
}

/// Implements winit-specific `AccessKit` functionality.
pub struct AccessKitPlugin;

impl Plugin for AccessKitPlugin {
    fn build(&self, app: &mut App) {
        app.init_resource::<WinitActionRequestHandlers>()
            .init_resource::<AccessibilityCompositionBoundary>()
            .init_resource::<AccessibilityConsumerFollowUp>()
            .add_message::<ActionRequestWrapper>()
            .add_message::<AccessibilitySubtreeActionRequest>()
            .add_message::<AccessibilitySubtreeLifecycle>()
            .add_systems(
                PostUpdate,
                update_accessibility_nodes
                    .run_if(should_update_accessibility_nodes)
                    // This is unlikely to result in real conflicts,
                    // as FocusChangeEvents only mutates internal state of InputFocus,
                    // and update_accessibility_nodes only reads from it.
                    // However, in case this changes in the future, this is a safer choice,
                    // as accessibility updates could conceivably want to read focus change events.
                    .after(bevy_input_focus::InputFocusSystems::FocusChangeEvents)
                    .in_set(AccessibilitySystems::Update),
            )
            .add_systems(
                PostUpdate,
                poll_receivers
                    .after(AccessibilitySystems::Update)
                    .in_set(AccessibilitySystems::Actions),
            )
            .add_systems(
                PostUpdate,
                flush_pending_lifecycle
                    .after(AccessibilitySystems::Update)
                    .before(AccessibilitySystems::Actions),
            )
            .add_systems(
                PostUpdate,
                begin_accessibility_owner_pass
                    .before(AccessibilitySystems::Update)
                    .before(AccessibilitySystems::Actions),
            )
            .add_systems(
                Last,
                (
                    window_closed.after(crate::system::despawn_windows),
                    request_accessibility_update_after_boundary,
                )
                    .chain()
                    .in_set(AccessibilitySystems::RequestUpdate),
            );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use accesskit::{Action, ActionData, ActionRequest, Affine, Point, TreeId, Uuid};
    use accesskit_consumer::{Node as ConsumerNode, Tree as ConsumerTree, TreeChangeHandler};
    use bevy_a11y::AccessibilityPlugin;
    use bevy_ecs::message::MessageCursor;
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct NoopConsumerChanges;

    impl TreeChangeHandler for NoopConsumerChanges {
        fn node_added(&mut self, _: &ConsumerNode) {}
        fn node_updated(&mut self, _: &ConsumerNode, _: &ConsumerNode) {}
        fn focus_moved(&mut self, _: Option<&ConsumerNode>, _: Option<&ConsumerNode>) {}
        fn node_removed(&mut self, _: &ConsumerNode) {}
    }

    fn consumer_tree(window: NodeId) -> ConsumerTree {
        ConsumerTree::new(
            TreeUpdate {
                nodes: vec![(window, Node::new(Role::Window))],
                tree: Some(Tree::new(window)),
                tree_id: TreeId::ROOT,
                focus: window,
            },
            true,
        )
    }

    fn consumer_root_update(
        window: NodeId,
        grafts: &[(NodeId, TreeId)],
        focus: NodeId,
    ) -> TreeUpdate {
        let mut window_node = Node::new(Role::Window);
        window_node.set_children(grafts.iter().map(|(node, _)| *node).collect::<Vec<_>>());
        let mut nodes = vec![(window, window_node)];
        nodes.extend(grafts.iter().map(|(node_id, tree_id)| {
            let mut node = Node::new(Role::GenericContainer);
            node.set_tree_id(*tree_id);
            (*node_id, node)
        }));
        TreeUpdate {
            nodes,
            tree: None,
            tree_id: TreeId::ROOT,
            focus,
        }
    }

    fn consumer_subtree_update(tree_id: TreeId, root: NodeId) -> TreeUpdate {
        TreeUpdate {
            nodes: vec![(root, Node::new(Role::Group))],
            tree: Some(Tree::new(root)),
            tree_id,
            focus: root,
        }
    }

    fn apply_consumer_update(tree: &mut ConsumerTree, update: TreeUpdate) {
        tree.update_and_process_changes(update, &mut NoopConsumerChanges);
    }

    fn consumer_root_focus(tree: &ConsumerTree) -> (NodeId, TreeId) {
        tree.state().focus_in_tree().locate()
    }

    fn request(tree: TreeId, node: NodeId) -> ActionRequest {
        ActionRequest {
            action: Action::Click,
            target_tree: tree,
            target_node: node,
            data: None,
        }
    }

    fn attached_provider(tree_id: TreeId) -> AttachedProvider {
        AttachedProvider {
            tree_id,
            attachment_generation: AccessibilityAttachmentGeneration(3),
            snapshot_sequence: Some(4),
            snapshot: None,
            notified_activation: Some(AccessibilityActivationGeneration(2)),
            published_activation: Some(AccessibilityActivationGeneration(2)),
            published_sequence: Some(4),
            detached: false,
            duplicate_invalid: false,
        }
    }

    fn subtree_snapshot(sequence: u64) -> AccessibilitySubtreeSnapshot {
        AccessibilitySubtreeSnapshot::try_from_full_update(
            sequence,
            TreeUpdate {
                nodes: vec![(NodeId(10), Node::new(Role::Group))],
                tree: Some(Tree::new(NodeId(10))),
                tree_id: TreeId::ROOT,
                focus: NodeId(10),
            },
        )
        .unwrap()
    }

    fn provider_input(entity: Entity, window: Entity, tree_id: TreeId) -> ProviderInput {
        ProviderInput {
            entity,
            provider: AccessibilitySubtreeProvider::new(window, tree_id).unwrap(),
            provider_tick: 0,
            provider_changed: false,
            snapshot: None,
            snapshot_tick: None,
            order: AccessibilitySubtreeOrder::default(),
            order_tick: None,
            transform: None,
            transform_tick: None,
            parent_focused: false,
            parent_focus_tick: None,
        }
    }

    fn emit_window_closed_once(mut closed: MessageWriter<WindowClosed>, mut emitted: Local<bool>) {
        if !*emitted {
            closed.write(WindowClosed {
                window: Entity::from_raw_u32(1).unwrap(),
            });
            *emitted = true;
        }
    }

    #[test]
    fn native_callbacks_coalesce_one_event_loop_wake_until_the_consumer_drains() {
        let wake_count = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&wake_count);
        let runtime = Arc::new(Mutex::new(WinitAccessibilityRuntime {
            adapter_generation: AccessibilityAdapterGeneration(1),
            wake_callback: Some(Arc::new(move || {
                counter.fetch_add(1, Ordering::Relaxed);
                true
            })),
            ..Default::default()
        }));
        let window_state = WindowAccessibilityState::default();
        let state = AccessKitState::new(
            "window",
            Entity::PLACEHOLDER,
            AccessibilityRequested::default(),
            window_state.clone(),
            Arc::clone(&runtime),
        );
        state.lock().unwrap().build_initial_tree();
        runtime.lock().unwrap().begin_owner_pass();
        let tree_id = TreeId(Uuid::from_u128(1));
        runtime.lock().unwrap().routes.insert(
            tree_id,
            RouteToken {
                provider: Entity::PLACEHOLDER,
                attachment_generation: AccessibilityAttachmentGeneration(1),
                snapshot_sequence: 1,
                committed_transform: None,
            },
        );
        WinitActionHandler::new(AccessibilityAdapterGeneration(1), Arc::clone(&runtime))
            .do_action(request(tree_id, NodeId(10)));
        WinitDeactivationHandler {
            window_state,
            runtime: Arc::clone(&runtime),
        }
        .deactivate_accessibility();
        assert_eq!(wake_count.load(Ordering::Relaxed), 1);

        runtime.lock().unwrap().acknowledge_owner_pass();
        assert_eq!(wake_count.load(Ordering::Relaxed), 2);
        let mut runtime = runtime.lock().unwrap();
        assert_eq!(runtime.consumed_callback_epoch, 1);
        assert_eq!(runtime.produced_callback_epoch, 3);
        assert_eq!(runtime.wake_sent_epoch, 3);
        runtime.begin_owner_pass();
        runtime.acknowledge_owner_pass();
        runtime.produce_callback();
        assert_eq!(wake_count.load(Ordering::Relaxed), 3);
    }

    #[test]
    fn callback_owned_activation_state_is_published_before_each_wake() {
        let requested = AccessibilityRequested::default();
        let window_state = WindowAccessibilityState::default();
        let expected_active = Arc::new(AtomicBool::new(true));
        let wake_count = Arc::new(AtomicUsize::new(0));
        let requested_at_wake = requested.clone();
        let state_at_wake = window_state.clone();
        let expected_at_wake = Arc::clone(&expected_active);
        let count_at_wake = Arc::clone(&wake_count);
        let runtime = Arc::new(Mutex::new(WinitAccessibilityRuntime {
            adapter_generation: AccessibilityAdapterGeneration(1),
            wake_callback: Some(Arc::new(move || {
                assert!(requested_at_wake.get());
                assert_eq!(
                    state_at_wake.is_active(),
                    expected_at_wake.load(Ordering::SeqCst)
                );
                count_at_wake.fetch_add(1, Ordering::SeqCst);
                true
            })),
            ..Default::default()
        }));
        let state = AccessKitState::new(
            "window",
            Entity::PLACEHOLDER,
            requested,
            window_state.clone(),
            Arc::clone(&runtime),
        );
        state.lock().unwrap().build_initial_tree();
        assert_eq!(wake_count.load(Ordering::SeqCst), 1);
        {
            let mut runtime = runtime.lock().unwrap();
            runtime.begin_owner_pass();
            runtime.acknowledge_owner_pass();
        }

        expected_active.store(false, Ordering::SeqCst);
        WinitDeactivationHandler {
            window_state,
            runtime,
        }
        .deactivate_accessibility();
        assert_eq!(wake_count.load(Ordering::SeqCst), 2);
    }

    #[test]
    fn forwarded_native_actions_request_exactly_one_consumer_follow_up_update() {
        let mut app = App::new();
        app.add_plugins((AccessibilityPlugin, AccessKitPlugin))
            .add_message::<RequestRedraw>()
            .add_message::<WindowClosed>();
        let window = Entity::from_raw_u32(1).unwrap();
        let runtime = Arc::new(Mutex::new(WinitAccessibilityRuntime {
            adapter_generation: AccessibilityAdapterGeneration(1),
            active: true,
            activation_generation: 1,
            actions: VecDeque::from([
                QueuedAction {
                    adapter_generation: AccessibilityAdapterGeneration(1),
                    activation_generation: AccessibilityActivationGeneration(1),
                    route: None,
                    request: request(TreeId::ROOT, NodeId(1)),
                },
                QueuedAction {
                    adapter_generation: AccessibilityAdapterGeneration(1),
                    activation_generation: AccessibilityActivationGeneration(1),
                    route: None,
                    request: request(TreeId::ROOT, NodeId(2)),
                },
            ]),
            ..Default::default()
        }));
        app.world_mut()
            .resource_mut::<WinitActionRequestHandlers>()
            .handlers
            .insert(window, runtime);

        let mut redraws = MessageCursor::<RequestRedraw>::default();
        let mut actions = MessageCursor::<ActionRequestWrapper>::default();
        app.update();
        assert_eq!(
            actions
                .read(app.world().resource::<Messages<ActionRequestWrapper>>())
                .count(),
            2
        );
        assert_eq!(
            redraws
                .read(app.world().resource::<Messages<RequestRedraw>>())
                .count(),
            1
        );
        app.update();
        assert_eq!(
            redraws
                .read(app.world().resource::<Messages<RequestRedraw>>())
                .count(),
            0
        );
    }

    #[test]
    fn lifecycle_notifications_coalesce_one_consumer_follow_up_update() {
        let mut app = App::new();
        app.add_plugins((AccessibilityPlugin, AccessKitPlugin))
            .add_message::<RequestRedraw>()
            .add_message::<WindowClosed>();
        let message = AccessibilitySubtreeLifecycle::Deactivated(lifecycle_token(
            Entity::from_raw_u32(1).unwrap(),
            Entity::from_raw_u32(2).unwrap(),
            TreeId(Uuid::from_u128(1)),
            AccessibilityAdapterGeneration(1),
            AccessibilityActivationGeneration(1),
            AccessibilityAttachmentGeneration(1),
        ));
        app.world_mut()
            .resource_mut::<WinitActionRequestHandlers>()
            .pending_lifecycle
            .extend([message, message]);

        let mut redraws = MessageCursor::<RequestRedraw>::default();
        let mut lifecycle = MessageCursor::<AccessibilitySubtreeLifecycle>::default();
        app.update();
        assert_eq!(
            lifecycle
                .read(
                    app.world()
                        .resource::<Messages<AccessibilitySubtreeLifecycle>>()
                )
                .count(),
            2
        );
        assert_eq!(
            redraws
                .read(app.world().resource::<Messages<RequestRedraw>>())
                .count(),
            1
        );
        app.update();
        assert_eq!(
            redraws
                .read(app.world().resource::<Messages<RequestRedraw>>())
                .count(),
            0
        );
    }

    #[test]
    fn last_window_close_retires_once_and_then_settles() {
        let mut app = App::new();
        app.add_plugins((AccessibilityPlugin, AccessKitPlugin))
            .add_message::<RequestRedraw>()
            .add_message::<WindowClosed>()
            .add_systems(Last, emit_window_closed_once.before(window_closed));
        let window = Entity::from_raw_u32(1).unwrap();
        let runtime = Arc::new(Mutex::new(WinitAccessibilityRuntime {
            adapter_generation: AccessibilityAdapterGeneration(1),
            active: true,
            activation_generation: 1,
            routes: HashMap::from_iter([(
                TreeId(Uuid::from_u128(1)),
                RouteToken {
                    provider: Entity::from_raw_u32(2).unwrap(),
                    attachment_generation: AccessibilityAttachmentGeneration(1),
                    snapshot_sequence: 1,
                    committed_transform: None,
                },
            )]),
            ..Default::default()
        }));
        app.world_mut()
            .resource_mut::<WinitActionRequestHandlers>()
            .handlers
            .insert(window, Arc::clone(&runtime));

        let mut redraws = MessageCursor::<RequestRedraw>::default();
        app.update();
        assert!(!app
            .world()
            .resource::<WinitActionRequestHandlers>()
            .handlers
            .contains_key(&window));
        let runtime = runtime.lock().unwrap();
        assert!(!runtime.active);
        assert!(runtime.routes.is_empty());
        drop(runtime);
        assert_eq!(
            redraws
                .read(app.world().resource::<Messages<RequestRedraw>>())
                .count(),
            1
        );

        app.update();
        assert_eq!(
            redraws
                .read(app.world().resource::<Messages<RequestRedraw>>())
                .count(),
            0
        );
    }

    fn emit_new_adapter_activation_once(
        mut handlers: ResMut<WinitActionRequestHandlers>,
        mut emitted: Local<bool>,
    ) {
        if !*emitted {
            handlers
                .pending_lifecycle
                .push(AccessibilitySubtreeLifecycle::Activated(lifecycle_token(
                    Entity::from_raw_u32(1).unwrap(),
                    Entity::from_raw_u32(2).unwrap(),
                    TreeId(Uuid::from_u128(1)),
                    AccessibilityAdapterGeneration(2),
                    AccessibilityActivationGeneration(1),
                    AccessibilityAttachmentGeneration(2),
                )));
            *emitted = true;
        }
    }

    #[test]
    fn adapter_recreation_orders_old_deactivation_before_new_activation_in_one_stream() {
        let mut app = App::new();
        app.add_plugins((AccessibilityPlugin, AccessKitPlugin))
            .add_message::<RequestRedraw>()
            .add_message::<WindowClosed>()
            .add_systems(
                PostUpdate,
                emit_new_adapter_activation_once.in_set(AccessibilitySystems::Update),
            );
        let old = AccessibilitySubtreeLifecycle::Deactivated(lifecycle_token(
            Entity::from_raw_u32(1).unwrap(),
            Entity::from_raw_u32(2).unwrap(),
            TreeId(Uuid::from_u128(1)),
            AccessibilityAdapterGeneration(1),
            AccessibilityActivationGeneration(1),
            AccessibilityAttachmentGeneration(1),
        ));
        app.world_mut()
            .resource_mut::<WinitActionRequestHandlers>()
            .pending_lifecycle
            .push(old);

        let mut cursor = MessageCursor::<AccessibilitySubtreeLifecycle>::default();
        app.update();
        let messages: Vec<_> = cursor
            .read(
                app.world()
                    .resource::<Messages<AccessibilitySubtreeLifecycle>>(),
            )
            .copied()
            .collect();
        assert_eq!(messages.len(), 2);
        assert!(matches!(
            messages[0],
            AccessibilitySubtreeLifecycle::Deactivated(token)
                if token.adapter_generation == AccessibilityAdapterGeneration(1)
        ));
        assert!(matches!(
            messages[1],
            AccessibilitySubtreeLifecycle::Activated(token)
                if token.adapter_generation == AccessibilityAdapterGeneration(2)
        ));
    }

    #[test]
    fn newer_snapshot_keeps_old_action_route_until_successful_commit() {
        let tree_id = TreeId(Uuid::from_u128(1));
        let old = RouteToken {
            provider: Entity::from_raw_u32(2).unwrap(),
            attachment_generation: AccessibilityAttachmentGeneration(1),
            snapshot_sequence: 1,
            committed_transform: None,
        };
        let new = RouteToken {
            attachment_generation: AccessibilityAttachmentGeneration(3),
            snapshot_sequence: 2,
            ..old
        };
        let mut runtime = WinitAccessibilityRuntime {
            adapter_generation: AccessibilityAdapterGeneration(1),
            active: true,
            activation_generation: 2,
            routes: HashMap::from_iter([(tree_id, old)]),
            ..Default::default()
        };
        let mut attached = attached_provider(tree_id);
        attached.snapshot = Some(subtree_snapshot(1));
        attached.snapshot_sequence = Some(1);
        let mut next_attachment_generation = 3;
        accept_provider_snapshot(
            &mut next_attachment_generation,
            &mut attached,
            &subtree_snapshot(2),
        );
        // Accepting the retained ECS snapshot is not a native commit, so the
        // prior successfully published route remains authoritative.
        assert_eq!(runtime.routes.get(&tree_id), Some(&old));
        assert!(commit_route_if_current(
            &mut runtime,
            AccessibilityAdapterGeneration(1),
            AccessibilityActivationGeneration(2),
            tree_id,
            RouteToken {
                attachment_generation: attached.attachment_generation,
                snapshot_sequence: attached.snapshot_sequence.unwrap(),
                ..new
            },
        ));
        assert_eq!(runtime.routes.get(&tree_id), Some(&new));
    }

    #[test]
    fn route_is_visible_to_reentrant_action_only_inside_current_publication_generation() {
        let window = Entity::from_raw_u32(1).unwrap();
        let provider = Entity::from_raw_u32(2).unwrap();
        let tree_id = TreeId(Uuid::from_u128(1));
        let route = RouteToken {
            provider,
            attachment_generation: AccessibilityAttachmentGeneration(1),
            snapshot_sequence: 1,
            committed_transform: None,
        };
        let runtime = Arc::new(Mutex::new(WinitAccessibilityRuntime {
            adapter_generation: AccessibilityAdapterGeneration(1),
            active: true,
            activation_generation: 2,
            ..Default::default()
        }));
        assert!(commit_route_if_current(
            &mut runtime.lock().unwrap(),
            AccessibilityAdapterGeneration(1),
            AccessibilityActivationGeneration(2),
            tree_id,
            route,
        ));
        WinitActionHandler::new(AccessibilityAdapterGeneration(1), Arc::clone(&runtime))
            .do_action(request(tree_id, NodeId(10)));
        let queued = runtime.lock().unwrap().actions.pop_front().unwrap();
        assert!(validate_subtree_action(window, &runtime.lock().unwrap(), queued).is_some());
    }

    #[test]
    fn route_commit_boundary_never_reinstates_deactivated_or_reactivated_generation() {
        let tree_id = TreeId(Uuid::from_u128(1));
        let route = RouteToken {
            provider: Entity::from_raw_u32(2).unwrap(),
            attachment_generation: AccessibilityAttachmentGeneration(1),
            snapshot_sequence: 1,
            committed_transform: None,
        };
        for reactivate in [false, true] {
            let mut runtime = WinitAccessibilityRuntime {
                adapter_generation: AccessibilityAdapterGeneration(1),
                active: true,
                activation_generation: 2,
                ..Default::default()
            };
            assert!(commit_route_if_current(
                &mut runtime,
                AccessibilityAdapterGeneration(1),
                AccessibilityActivationGeneration(2),
                tree_id,
                route,
            ));
            suspend_routes_for_transaction(&mut runtime, core::iter::once(tree_id));
            runtime.active = reactivate;
            if reactivate {
                runtime.activation_generation = 3;
            }
            assert!(!commit_route_if_current(
                &mut runtime,
                AccessibilityAdapterGeneration(1),
                AccessibilityActivationGeneration(2),
                tree_id,
                route,
            ));
            assert!(!runtime.routes.contains_key(&tree_id));
        }
    }

    #[test]
    fn activation_change_between_root_and_subtree_submits_only_safe_root_then_replays() {
        let tree_id = TreeId(Uuid::from_u128(1));
        let route = RouteToken {
            provider: Entity::from_raw_u32(2).unwrap(),
            attachment_generation: AccessibilityAttachmentGeneration(1),
            snapshot_sequence: 1,
            committed_transform: None,
        };
        let update = |tree_id| TreeUpdate {
            nodes: vec![(NodeId(1), Node::new(Role::Group))],
            tree: (tree_id != TreeId::ROOT).then(|| Tree::new(NodeId(1))),
            tree_id,
            focus: NodeId(1),
        };
        let mut runtime = WinitAccessibilityRuntime {
            adapter_generation: AccessibilityAdapterGeneration(1),
            active: true,
            activation_generation: 3,
            ..Default::default()
        };
        let (submitted, committed) = select_generation_safe_publication(
            &runtime,
            AccessibilityAdapterGeneration(1),
            AccessibilityActivationGeneration(2),
            update(tree_id),
            || update(TreeId::ROOT),
        );
        assert_eq!(submitted.tree_id, TreeId::ROOT);
        assert!(!committed);
        assert!(runtime.routes.is_empty());

        let (replayed, committed) = select_generation_safe_publication(
            &runtime,
            AccessibilityAdapterGeneration(1),
            AccessibilityActivationGeneration(3),
            update(tree_id),
            || update(TreeId::ROOT),
        );
        assert_eq!(replayed.tree_id, tree_id);
        assert!(committed);
        assert!(runtime.routes.is_empty());
        assert!(commit_route_if_current(
            &mut runtime,
            AccessibilityAdapterGeneration(1),
            AccessibilityActivationGeneration(3),
            tree_id,
            route,
        ));
        assert_eq!(runtime.routes.get(&tree_id), Some(&route));
    }

    #[test]
    fn full_provider_composition_bootstraps_without_snapshot_then_balances_detach_and_readd() {
        let tree_id = TreeId(Uuid::from_u128(1));
        let mut attached = attached_provider(tree_id);
        attached.snapshot_sequence = None;
        attached.snapshot = None;
        attached.notified_activation = None;
        let mut runtime = WinitAccessibilityRuntime {
            active: true,
            activation_generation: 2,
            ..Default::default()
        };
        let mut next_generation = 3;
        assert_eq!(
            compose_provider_lifecycle(
                &mut next_generation,
                &mut runtime,
                &mut attached,
                None,
                true,
                &[],
            ),
            vec![ProviderLifecycleEmission::Activated {
                activation_generation: AccessibilityActivationGeneration(2),
                attachment_generation: AccessibilityAttachmentGeneration(3),
            }]
        );
        assert!(attached.snapshot.is_none());
        assert!(compose_provider_lifecycle(
            &mut next_generation,
            &mut runtime,
            &mut attached,
            Some(&subtree_snapshot(4)),
            true,
            &[],
        )
        .is_empty());
        assert!(attached.snapshot.is_some());
        let route = RouteToken {
            provider: Entity::from_raw_u32(2).unwrap(),
            attachment_generation: attached.attachment_generation,
            snapshot_sequence: attached.snapshot_sequence.unwrap(),
            committed_transform: None,
        };
        let (published, committed) = select_generation_safe_publication(
            &runtime,
            AccessibilityAdapterGeneration(0),
            AccessibilityActivationGeneration(2),
            materialize_snapshot(attached.snapshot.as_ref().unwrap(), tree_id),
            || unreachable!("the bootstrap generation is current"),
        );
        assert!(committed);
        assert_eq!(published.tree_id, tree_id);
        assert!(commit_route_if_current(
            &mut runtime,
            AccessibilityAdapterGeneration(0),
            AccessibilityActivationGeneration(2),
            tree_id,
            route,
        ));
        assert_eq!(runtime.routes.get(&tree_id), Some(&route));
        assert_eq!(
            compose_provider_lifecycle(
                &mut next_generation,
                &mut runtime,
                &mut attached,
                None,
                true,
                &[],
            ),
            vec![ProviderLifecycleEmission::Deactivated {
                activation_generation: AccessibilityActivationGeneration(2),
                attachment_generation: AccessibilityAttachmentGeneration(3),
            }]
        );
        assert!(attached.detached);
        assert!(compose_provider_lifecycle(
            &mut next_generation,
            &mut runtime,
            &mut attached,
            None,
            true,
            &[],
        )
        .is_empty());

        assert_eq!(
            compose_provider_lifecycle(
                &mut next_generation,
                &mut runtime,
                &mut attached,
                Some(&subtree_snapshot(5)),
                true,
                &[],
            ),
            vec![ProviderLifecycleEmission::Activated {
                activation_generation: AccessibilityActivationGeneration(2),
                attachment_generation: AccessibilityAttachmentGeneration(4),
            }]
        );
        assert_eq!(next_generation, 4);
        assert_eq!(
            attached.attachment_generation,
            AccessibilityAttachmentGeneration(4)
        );
        assert!(!attached.detached);
    }

    #[test]
    fn rapid_deactivate_reactivate_emits_old_deactivation_before_new_activation() {
        let tree_id = TreeId(Uuid::from_u128(1));
        let mut attached = attached_provider(tree_id);
        attached.snapshot = Some(subtree_snapshot(1));
        attached.notified_activation = Some(AccessibilityActivationGeneration(1));
        let mut runtime = WinitAccessibilityRuntime {
            active: true,
            activation_generation: 2,
            ..Default::default()
        };
        let mut next_generation = 3;
        assert_eq!(
            compose_provider_lifecycle(
                &mut next_generation,
                &mut runtime,
                &mut attached,
                Some(&subtree_snapshot(1)),
                true,
                &[AccessibilityActivationGeneration(1)],
            ),
            vec![
                ProviderLifecycleEmission::Deactivated {
                    activation_generation: AccessibilityActivationGeneration(1),
                    attachment_generation: AccessibilityAttachmentGeneration(3),
                },
                ProviderLifecycleEmission::Activated {
                    activation_generation: AccessibilityActivationGeneration(2),
                    attachment_generation: AccessibilityAttachmentGeneration(3),
                },
            ]
        );
    }

    #[test]
    fn retained_snapshot_is_cheap_and_materialized_once_only_for_native_publication() {
        SNAPSHOT_MATERIALIZATIONS.set(0);
        let tree_id = TreeId(Uuid::from_u128(1));
        let snapshot = subtree_snapshot(1);
        let mut attached = attached_provider(tree_id);
        attached.snapshot_sequence = None;
        attached.snapshot = None;
        attached.notified_activation = None;
        let mut runtime = WinitAccessibilityRuntime {
            active: true,
            activation_generation: 1,
            ..Default::default()
        };
        let mut next_generation = 3;
        let _ = compose_provider_lifecycle(
            &mut next_generation,
            &mut runtime,
            &mut attached,
            Some(&snapshot),
            true,
            &[],
        );
        let _cheap_retained_clone = attached.snapshot.clone();
        assert_eq!(SNAPSHOT_MATERIALIZATIONS.get(), 0);
        let update = materialize_snapshot(attached.snapshot.as_ref().unwrap(), tree_id);
        assert_eq!(update.tree_id, tree_id);
        assert_eq!(SNAPSHOT_MATERIALIZATIONS.get(), 1);
    }

    #[test]
    fn graft_node_id_collisions_with_window_or_primary_ecs_node_are_rejected() {
        let window = Entity::from_raw_u32(1).unwrap();
        let provider = Entity::from_raw_u32(2).unwrap();
        assert!(!graft_node_id_is_available(window, window, false));
        assert!(!graft_node_id_is_available(provider, window, true));
        assert!(graft_node_id_is_available(provider, window, false));
    }

    #[test]
    fn two_windows_may_reuse_the_same_toolkit_local_tree_id() {
        let first_window = Entity::from_raw_u32(1).unwrap();
        let second_window = Entity::from_raw_u32(2).unwrap();
        let tree_id = TreeId(Uuid::from_u128(1));
        let first = provider_input(Entity::from_raw_u32(3).unwrap(), first_window, tree_id);
        let second = provider_input(Entity::from_raw_u32(4).unwrap(), second_window, tree_id);
        let all = [&first, &second];
        for window in [first_window, second_window] {
            let window_inputs: Vec<_> = all
                .iter()
                .copied()
                .filter(|input| input.provider.window() == window)
                .collect();
            assert_eq!(count_tree_ids(&window_inputs).get(&tree_id), Some(&1));
        }
    }

    #[test]
    fn removed_provider_is_pruned_from_invalid_graft_diagnostics() {
        let window = Entity::from_raw_u32(1).unwrap();
        let retained = provider_input(
            Entity::from_raw_u32(2).unwrap(),
            window,
            TreeId(Uuid::from_u128(1)),
        );
        let removed = Entity::from_raw_u32(3).unwrap();
        let mut invalid = EntityHashSet::from_iter([retained.entity, removed]);
        prune_invalid_grafts(&mut invalid, &[&retained]);
        assert!(invalid.contains(&retained.entity));
        assert!(!invalid.contains(&removed));
    }

    #[test]
    fn native_action_captures_and_validates_every_generation() {
        let window = Entity::from_raw_u32(1).unwrap();
        let provider = Entity::from_raw_u32(2).unwrap();
        let tree = TreeId(Uuid::from_u128(1));
        let route = RouteToken {
            provider,
            attachment_generation: AccessibilityAttachmentGeneration(3),
            snapshot_sequence: 4,
            committed_transform: None,
        };
        let runtime = Arc::new(Mutex::new(WinitAccessibilityRuntime {
            adapter_generation: AccessibilityAdapterGeneration(1),
            active: true,
            activation_generation: 2,
            routes: HashMap::from_iter([(tree, route)]),
            actions: VecDeque::new(),
            wake_callback: None,
            ..Default::default()
        }));
        let mut handler =
            WinitActionHandler::new(AccessibilityAdapterGeneration(1), Arc::clone(&runtime));
        handler.do_action(request(tree, NodeId(9)));
        let queued = runtime.lock().unwrap().actions.pop_front().unwrap();
        let routed = validate_subtree_action(window, &runtime.lock().unwrap(), queued.clone())
            .expect("all captured generations match");
        assert_eq!(routed.provider, provider);
        assert_eq!(routed.snapshot_sequence, 4);

        runtime.lock().unwrap().adapter_generation = AccessibilityAdapterGeneration(2);
        assert!(
            validate_subtree_action(window, &runtime.lock().unwrap(), queued.clone()).is_none()
        );
        runtime.lock().unwrap().adapter_generation = AccessibilityAdapterGeneration(1);
        runtime.lock().unwrap().activation_generation = 3;
        assert!(
            validate_subtree_action(window, &runtime.lock().unwrap(), queued.clone()).is_none()
        );
        runtime.lock().unwrap().activation_generation = 2;
        runtime.lock().unwrap().routes.remove(&tree);
        assert!(validate_subtree_action(window, &runtime.lock().unwrap(), queued).is_none());
    }

    #[test]
    fn queued_action_keeps_the_transform_committed_when_the_callback_arrived() {
        let window = Entity::from_raw_u32(1).unwrap();
        let provider = Entity::from_raw_u32(2).unwrap();
        let tree = TreeId(Uuid::from_u128(1));
        let transform_a = AccessibilitySubtreeTransform(Affine::translate((20.0, 30.0)));
        let transform_b = AccessibilitySubtreeTransform(Affine::translate((100.0, 200.0)));
        let route_a = RouteToken {
            provider,
            attachment_generation: AccessibilityAttachmentGeneration(3),
            snapshot_sequence: 4,
            committed_transform: Some(transform_a),
        };
        let runtime = Arc::new(Mutex::new(WinitAccessibilityRuntime {
            adapter_generation: AccessibilityAdapterGeneration(1),
            active: true,
            activation_generation: 2,
            routes: HashMap::from_iter([(tree, route_a)]),
            ..Default::default()
        }));
        WinitActionHandler::new(AccessibilityAdapterGeneration(1), Arc::clone(&runtime)).do_action(
            ActionRequest {
                action: Action::ScrollToPoint,
                target_tree: tree,
                target_node: NodeId(9),
                data: Some(ActionData::ScrollToPoint(Point::new(45.0, 70.0))),
            },
        );
        let queued_under_a = runtime.lock().unwrap().actions.pop_front().unwrap();

        let route_b = RouteToken {
            committed_transform: Some(transform_b),
            ..route_a
        };
        suspend_routes_for_transaction(&mut runtime.lock().unwrap(), core::iter::once(tree));
        let (evaluated, valid) = select_generation_safe_publication(
            &runtime.lock().unwrap(),
            AccessibilityAdapterGeneration(1),
            AccessibilityActivationGeneration(2),
            TreeUpdate {
                nodes: Vec::new(),
                tree: None,
                tree_id: TreeId::ROOT,
                focus: NodeId(1),
            },
            || unreachable!("the modeled ROOT transaction is current"),
        );
        assert!(valid);
        assert_eq!(evaluated.tree_id, TreeId::ROOT);
        WinitActionHandler::new(AccessibilityAdapterGeneration(1), Arc::clone(&runtime))
            .do_action(request(tree, NodeId(9)));
        assert!(
            runtime.lock().unwrap().actions.is_empty(),
            "an action emitted after factory evaluation but before commit must see no route"
        );
        assert!(commit_route_if_current(
            &mut runtime.lock().unwrap(),
            AccessibilityAdapterGeneration(1),
            AccessibilityActivationGeneration(2),
            tree,
            route_b,
        ));
        assert_eq!(runtime.lock().unwrap().routes.get(&tree), Some(&route_b));

        let routed = validate_subtree_action(window, &runtime.lock().unwrap(), queued_under_a)
            .expect("a transform-only root commit keeps attachment and snapshot identity valid");
        assert_eq!(routed.committed_transform, Some(transform_a));
        let Some(ActionData::ScrollToPoint(parent_point)) = routed.request.data else {
            panic!("expected ScrollToPoint payload");
        };
        assert_eq!(
            transform_a.0.inverse() * parent_point,
            Point::new(25.0, 40.0)
        );
    }

    #[test]
    fn deactivation_drops_queued_root_and_subtree_actions_before_poll() {
        let provider = Entity::from_raw_u32(2).unwrap();
        let tree_id = TreeId(Uuid::from_u128(1));
        let route = RouteToken {
            provider,
            attachment_generation: AccessibilityAttachmentGeneration(1),
            snapshot_sequence: 1,
            committed_transform: None,
        };
        let runtime = Arc::new(Mutex::new(WinitAccessibilityRuntime {
            adapter_generation: AccessibilityAdapterGeneration(1),
            active: true,
            activation_generation: 1,
            routes: HashMap::from_iter([(tree_id, route)]),
            ..Default::default()
        }));
        let mut handler =
            WinitActionHandler::new(AccessibilityAdapterGeneration(1), Arc::clone(&runtime));
        handler.do_action(request(TreeId::ROOT, NodeId(1)));
        handler.do_action(request(tree_id, NodeId(2)));
        let queued: Vec<_> = runtime.lock().unwrap().actions.iter().cloned().collect();
        assert_eq!(queued.len(), 2);

        WinitDeactivationHandler {
            window_state: WindowAccessibilityState::default(),
            runtime: Arc::clone(&runtime),
        }
        .deactivate_accessibility();
        let runtime = runtime.lock().unwrap();
        assert!(runtime.actions.is_empty());
        assert!(queued
            .iter()
            .all(|queued| !queued_action_is_current(&runtime, queued)));
    }

    #[test]
    fn consecutive_activation_advances_generation_and_clears_routes() {
        let requested = AccessibilityRequested::default();
        let window_state = WindowAccessibilityState::default();
        let runtime = Arc::new(Mutex::new(WinitAccessibilityRuntime {
            adapter_generation: AccessibilityAdapterGeneration(1),
            ..Default::default()
        }));
        runtime.lock().unwrap().routes.insert(
            TreeId(Uuid::from_u128(1)),
            RouteToken {
                provider: Entity::PLACEHOLDER,
                attachment_generation: AccessibilityAttachmentGeneration(1),
                snapshot_sequence: 1,
                committed_transform: None,
            },
        );
        let state = AccessKitState::new(
            "window",
            Entity::PLACEHOLDER,
            requested,
            window_state.clone(),
            Arc::clone(&runtime),
        );
        state.lock().unwrap().build_initial_tree();
        assert_eq!(runtime.lock().unwrap().activation_generation, 1);
        assert!(runtime.lock().unwrap().routes.is_empty());
        runtime.lock().unwrap().routes.insert(
            TreeId(Uuid::from_u128(2)),
            RouteToken {
                provider: Entity::PLACEHOLDER,
                attachment_generation: AccessibilityAttachmentGeneration(1),
                snapshot_sequence: 1,
                committed_transform: None,
            },
        );
        state.lock().unwrap().build_initial_tree();
        assert_eq!(runtime.lock().unwrap().activation_generation, 2);
        assert!(runtime.lock().unwrap().routes.is_empty());
        assert!(window_state.is_active());
    }

    #[test]
    fn deactivation_is_emitted_once_per_activated_provider() {
        let provider = Entity::from_raw_u32(2).unwrap();
        let window = Entity::from_raw_u32(1).unwrap();
        let mut providers = EntityHashMap::default();
        providers.insert(
            provider,
            AttachedProvider {
                tree_id: TreeId(Uuid::from_u128(1)),
                attachment_generation: AccessibilityAttachmentGeneration(3),
                snapshot_sequence: None,
                snapshot: None,
                notified_activation: Some(AccessibilityActivationGeneration(2)),
                published_activation: None,
                published_sequence: None,
                detached: false,
                duplicate_invalid: false,
            },
        );
        let first =
            take_provider_deactivations(window, AccessibilityAdapterGeneration(1), &mut providers);
        assert_eq!(first.len(), 1);
        assert!(matches!(
            first[0],
            AccessibilitySubtreeLifecycle::Deactivated(token) if token.provider == provider
        ));
        assert!(take_provider_deactivations(
            window,
            AccessibilityAdapterGeneration(1),
            &mut providers,
        )
        .is_empty());
    }

    #[test]
    fn adapter_recreation_retires_old_routes_actions_and_provider_activations() {
        let window = Entity::from_raw_u32(1).unwrap();
        let provider = Entity::from_raw_u32(2).unwrap();
        let tree_id = TreeId(Uuid::from_u128(1));
        let route = RouteToken {
            provider,
            attachment_generation: AccessibilityAttachmentGeneration(3),
            snapshot_sequence: 4,
            committed_transform: None,
        };
        let runtime = Arc::new(Mutex::new(WinitAccessibilityRuntime {
            adapter_generation: AccessibilityAdapterGeneration(1),
            active: true,
            routes: HashMap::from_iter([(tree_id, route)]),
            actions: VecDeque::from([QueuedAction {
                adapter_generation: AccessibilityAdapterGeneration(1),
                activation_generation: AccessibilityActivationGeneration(2),
                route: Some(route),
                request: request(tree_id, NodeId(10)),
            }]),
            ..Default::default()
        }));
        let mut providers = EntityHashMap::from_iter([(provider, attached_provider(tree_id))]);
        let window_state = WindowAccessibilityState::default();
        window_state.set_platform_active(true);
        let deactivated = retire_adapter_state(
            window,
            AccessibilityAdapterGeneration(1),
            &runtime,
            &window_state,
            &mut providers,
            RuntimeRetirement::WakeOwner,
        );
        assert_eq!(deactivated.len(), 1);
        assert!(matches!(
            deactivated[0],
            AccessibilitySubtreeLifecycle::Deactivated(token) if token.provider == provider
        ));
        let runtime = runtime.lock().unwrap();
        assert!(!runtime.active);
        assert!(runtime.routes.is_empty());
        assert!(runtime.actions.is_empty());
        assert!(!window_state.is_active());
    }

    #[test]
    fn closed_adapter_retirement_emits_deactivation_without_proxy_wake() {
        let window = Entity::from_raw_u32(1).unwrap();
        let provider = Entity::from_raw_u32(2).unwrap();
        let tree_id = TreeId(Uuid::from_u128(1));
        let wake_count = Arc::new(AtomicUsize::new(0));
        let wake_counter = Arc::clone(&wake_count);
        let runtime = Arc::new(Mutex::new(WinitAccessibilityRuntime {
            adapter_generation: AccessibilityAdapterGeneration(1),
            active: true,
            activation_generation: 2,
            wake_callback: Some(Arc::new(move || {
                wake_counter.fetch_add(1, Ordering::SeqCst);
                true
            })),
            ..Default::default()
        }));
        let mut providers = EntityHashMap::from_iter([(provider, attached_provider(tree_id))]);
        let window_state = WindowAccessibilityState::default();
        window_state.set_platform_active(true);

        let lifecycle = retire_adapter_state(
            window,
            AccessibilityAdapterGeneration(1),
            &runtime,
            &window_state,
            &mut providers,
            RuntimeRetirement::Disconnect,
        );
        assert_eq!(lifecycle.len(), 1);
        assert!(matches!(
            lifecycle[0],
            AccessibilitySubtreeLifecycle::Deactivated(token)
                if token.provider == provider
                    && token.activation_generation == AccessibilityActivationGeneration(2)
        ));
        assert_eq!(wake_count.load(Ordering::SeqCst), 0);
        let runtime = runtime.lock().unwrap();
        assert!(!runtime.active);
        assert!(runtime.wake_callback.is_none());
        assert!(!window_state.is_active());
    }

    #[test]
    fn terminal_shutdown_clears_runtime_handlers_state_and_proxies_without_wake() {
        ACCESS_KIT_ADAPTERS.with_borrow_mut(|adapters| adapters.clear());
        let window_state = WindowAccessibilityState::default();
        window_state.set_platform_active(true);
        let state_runtime = Arc::new(Mutex::new(WinitAccessibilityRuntime {
            active: true,
            ..Default::default()
        }));
        retire_runtime(
            &state_runtime,
            Some(&window_state),
            RuntimeRetirement::Disconnect,
        );
        assert!(!window_state.is_active());

        let wake_count = Arc::new(AtomicUsize::new(0));
        let wake_counter = Arc::clone(&wake_count);
        let runtime = Arc::new(Mutex::new(WinitAccessibilityRuntime {
            adapter_generation: AccessibilityAdapterGeneration(1),
            active: true,
            activation_generation: 1,
            pending_deactivation_generations: VecDeque::from([AccessibilityActivationGeneration(
                1,
            )]),
            routes: HashMap::from_iter([(
                TreeId(Uuid::from_u128(1)),
                RouteToken {
                    provider: Entity::from_raw_u32(2).unwrap(),
                    attachment_generation: AccessibilityAttachmentGeneration(1),
                    snapshot_sequence: 1,
                    committed_transform: None,
                },
            )]),
            wake_callback: Some(Arc::new(move || {
                wake_counter.fetch_add(1, Ordering::SeqCst);
                true
            })),
            ..Default::default()
        }));

        let mut app = App::new();
        app.add_plugins((AccessibilityPlugin, AccessKitPlugin))
            .add_message::<WindowClosed>();
        let token = lifecycle_token(
            Entity::from_raw_u32(1).unwrap(),
            Entity::from_raw_u32(2).unwrap(),
            TreeId(Uuid::from_u128(1)),
            AccessibilityAdapterGeneration(1),
            AccessibilityActivationGeneration(1),
            AccessibilityAttachmentGeneration(1),
        );
        {
            let mut handlers = app.world_mut().resource_mut::<WinitActionRequestHandlers>();
            handlers
                .handlers
                .insert(Entity::from_raw_u32(1).unwrap(), Arc::clone(&runtime));
            handlers
                .pending_lifecycle
                .push(AccessibilitySubtreeLifecycle::Activated(token));
        }
        shutdown_accessibility(app.world_mut());

        let handlers = app.world().resource::<WinitActionRequestHandlers>();
        assert!(handlers.handlers.is_empty());
        assert!(handlers.pending_lifecycle.is_empty());
        ACCESS_KIT_ADAPTERS.with_borrow(|adapters| assert!(adapters.is_empty()));
        let runtime = runtime.lock().unwrap();
        assert!(!runtime.active);
        assert!(runtime.routes.is_empty());
        assert!(runtime.actions.is_empty());
        assert!(runtime.pending_deactivation_generations.is_empty());
        assert!(runtime.wake_callback.is_none());
        assert_eq!(wake_count.load(Ordering::SeqCst), 0);
        assert!(!window_state.is_active());
    }

    #[test]
    fn duplicate_tree_id_advances_attachment_and_forces_replay_after_resolution() {
        let provider = Entity::from_raw_u32(2).unwrap();
        let tree_id = TreeId(Uuid::from_u128(1));
        let route = RouteToken {
            provider,
            attachment_generation: AccessibilityAttachmentGeneration(3),
            snapshot_sequence: 4,
            committed_transform: None,
        };
        let mut runtime = WinitAccessibilityRuntime {
            routes: HashMap::from_iter([(tree_id, route)]),
            ..Default::default()
        };
        let mut attached = attached_provider(tree_id);
        let mut next_attachment_generation = 3;
        assert_eq!(
            invalidate_duplicate_attachment(
                &mut next_attachment_generation,
                &mut runtime,
                &mut attached
            ),
            (
                AccessibilityAttachmentGeneration(3),
                Some(AccessibilityActivationGeneration(2))
            )
        );
        assert_eq!(next_attachment_generation, 4);
        assert_eq!(
            attached.attachment_generation,
            AccessibilityAttachmentGeneration(4)
        );
        assert!(runtime.routes.is_empty());
        assert_eq!(attached.published_activation, None);
        assert_eq!(attached.published_sequence, None);
        assert_eq!(
            invalidate_duplicate_attachment(
                &mut next_attachment_generation,
                &mut runtime,
                &mut attached
            )
            .0,
            AccessibilityAttachmentGeneration(4)
        );
        assert_eq!(next_attachment_generation, 4);
    }

    #[test]
    fn conflicting_parent_focus_claims_fail_closed() {
        let first = Entity::from_raw_u32(1).unwrap();
        let second = Entity::from_raw_u32(2).unwrap();
        assert_eq!(resolve_parent_focus(&[]), None);
        assert_eq!(resolve_parent_focus(&[first]), Some(first));
        assert_eq!(resolve_parent_focus(&[first, second]), None);
    }

    #[test]
    fn composition_batch_is_root_first_then_subtrees() {
        let update = |tree_id| TreeUpdate {
            nodes: vec![(NodeId(1), Node::new(Role::Group))],
            tree: Some(Tree::new(NodeId(1))),
            tree_id,
            focus: NodeId(1),
        };
        let first = TreeId(Uuid::from_u128(1));
        let second = TreeId(Uuid::from_u128(2));
        let tree_ids = root_first_updates(update(TreeId::ROOT), [update(first), update(second)])
            .map(|update| update.tree_id)
            .collect::<Vec<_>>();
        assert_eq!(tree_ids, [TreeId::ROOT, first, second]);
    }

    #[test]
    fn accesskit_consumer_accepts_initial_focused_provider_transaction() {
        let window = NodeId(1);
        let graft = NodeId(2);
        let subtree_root = NodeId(10);
        let tree_id = TreeId(Uuid::from_u128(1));
        let mut consumer = consumer_tree(window);

        apply_consumer_update(
            &mut consumer,
            consumer_root_update(window, &[(graft, tree_id)], window),
        );
        assert_eq!(consumer_root_focus(&consumer), (window, TreeId::ROOT));
        apply_consumer_update(
            &mut consumer,
            consumer_subtree_update(tree_id, subtree_root),
        );
        apply_consumer_update(
            &mut consumer,
            consumer_root_update(window, &[(graft, tree_id)], graft),
        );
        assert_eq!(consumer_root_focus(&consumer), (subtree_root, tree_id));
        assert!(consumer.state().subtree_root(tree_id).is_some());
    }

    #[test]
    fn accesskit_consumer_accepts_reactivation_replay_transaction() {
        let window = NodeId(1);
        let graft = NodeId(2);
        let tree_id = TreeId(Uuid::from_u128(1));
        for _activation in 0..2 {
            let mut consumer = consumer_tree(window);
            apply_consumer_update(
                &mut consumer,
                consumer_root_update(window, &[(graft, tree_id)], window),
            );
            apply_consumer_update(&mut consumer, consumer_subtree_update(tree_id, NodeId(10)));
            apply_consumer_update(
                &mut consumer,
                consumer_root_update(window, &[(graft, tree_id)], graft),
            );
            assert_eq!(consumer_root_focus(&consumer), (NodeId(10), tree_id));
        }
    }

    #[test]
    fn accesskit_consumer_accepts_snapshot_detach_and_reattach_transaction() {
        let window = NodeId(1);
        let graft = NodeId(2);
        let tree_id = TreeId(Uuid::from_u128(1));
        let mut consumer = consumer_tree(window);
        apply_consumer_update(
            &mut consumer,
            consumer_root_update(window, &[(graft, tree_id)], window),
        );
        apply_consumer_update(&mut consumer, consumer_subtree_update(tree_id, NodeId(10)));
        apply_consumer_update(
            &mut consumer,
            consumer_root_update(window, &[(graft, tree_id)], graft),
        );

        apply_consumer_update(&mut consumer, consumer_root_update(window, &[], window));
        assert_eq!(consumer_root_focus(&consumer), (window, TreeId::ROOT));
        assert!(consumer.state().subtree_root(tree_id).is_none());

        apply_consumer_update(
            &mut consumer,
            consumer_root_update(window, &[(graft, tree_id)], window),
        );
        apply_consumer_update(&mut consumer, consumer_subtree_update(tree_id, NodeId(11)));
        apply_consumer_update(
            &mut consumer,
            consumer_root_update(window, &[(graft, tree_id)], graft),
        );
        assert_eq!(consumer_root_focus(&consumer), (NodeId(11), tree_id));
    }

    #[test]
    fn accesskit_consumer_focus_handoff_keeps_a_until_new_b_subtree_exists() {
        let window = NodeId(1);
        let graft_a = NodeId(2);
        let graft_b = NodeId(3);
        let tree_a = TreeId(Uuid::from_u128(1));
        let tree_b = TreeId(Uuid::from_u128(2));
        let mut consumer = consumer_tree(window);
        apply_consumer_update(
            &mut consumer,
            consumer_root_update(window, &[(graft_a, tree_a)], window),
        );
        apply_consumer_update(&mut consumer, consumer_subtree_update(tree_a, NodeId(10)));
        apply_consumer_update(
            &mut consumer,
            consumer_root_update(window, &[(graft_a, tree_a)], graft_a),
        );

        apply_consumer_update(
            &mut consumer,
            consumer_root_update(window, &[(graft_a, tree_a), (graft_b, tree_b)], graft_a),
        );
        assert_eq!(consumer_root_focus(&consumer), (NodeId(10), tree_a));
        apply_consumer_update(&mut consumer, consumer_subtree_update(tree_b, NodeId(20)));
        apply_consumer_update(
            &mut consumer,
            consumer_root_update(window, &[(graft_a, tree_a), (graft_b, tree_b)], graft_b),
        );
        assert_eq!(consumer_root_focus(&consumer), (NodeId(20), tree_b));
    }

    fn insert_parent_focus_after_owner(
        mut commands: Commands,
        providers: Query<Entity, With<AccessibilitySubtreeProvider>>,
        mut inserted: Local<bool>,
    ) {
        if !*inserted {
            commands
                .entity(providers.single().unwrap())
                .insert(AccessibilitySubtreeParentFocus);
            *inserted = true;
        }
    }

    #[test]
    fn provider_change_after_owner_requests_exactly_one_follow_up() {
        let mut app = App::new();
        app.add_plugins((AccessibilityPlugin, AccessKitPlugin))
            .add_message::<RequestRedraw>()
            .add_message::<WindowClosed>()
            .add_systems(
                PostUpdate,
                insert_parent_focus_after_owner.after(AccessibilitySystems::Update),
            );
        app.world().resource::<AccessibilityRequested>().set(true);
        let window = app.world_mut().spawn(Window::default()).id();
        app.world_mut()
            .spawn(AccessibilitySubtreeProvider::new(window, TreeId(Uuid::from_u128(1))).unwrap());

        let mut cursor = MessageCursor::<RequestRedraw>::default();
        app.update();
        assert_eq!(
            cursor
                .read(app.world().resource::<Messages<RequestRedraw>>())
                .count(),
            1
        );
        app.update();
        assert_eq!(
            cursor
                .read(app.world().resource::<Messages<RequestRedraw>>())
                .count(),
            0
        );
    }
}
