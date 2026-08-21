#![forbid(unsafe_code)]
#![cfg_attr(docsrs, feature(doc_cfg))]
#![doc(
    html_logo_url = "https://bevy.org/assets/icon.png",
    html_favicon_url = "https://bevy.org/assets/icon.png"
)]
#![no_std]

//! Reusable accessibility primitives
//!
//! This crate provides accessibility integration for the engine. It exposes the
//! [`AccessibilityPlugin`]. This plugin integrates `AccessKit`, a Rust crate
//! providing OS-agnostic accessibility primitives, with Bevy's ECS.
//!
//! ## Some notes on utility
//!
//! While this crate defines useful types for accessibility, it does not
//! actually power accessibility features in Bevy.
//!
//! Instead, it helps other interfaces coordinate their approach to
//! accessibility. Binary authors should add the [`AccessibilityPlugin`], while
//! library maintainers may use the [`AccessibilityRequested`] and
//! [`ManageAccessibilityUpdates`] resources.
//!
//! The [`AccessibilityNode`] component is useful in both cases. It helps
//! describe an entity in terms of its accessibility factors through an
//! `AccessKit` "node".
//!
//! Typical UI concepts, like buttons, checkboxes, and textboxes, are easily
//! described by this component, though, technically, it can represent any kind
//! of Bevy [`Entity`].
//!
//! ## This crate no longer re-exports `AccessKit`
//!
//! As of Bevy version 0.15, [the `accesskit` crate][accesskit_crate] is no
//! longer re-exported from this crate.[^accesskit_node_confusion] If you need
//! to use `AccessKit` yourself, you'll have to add it as a separate dependency
//! in your project's `Cargo.toml`.
//!
//! Make sure to use the same version of the `accesskit` crate as Bevy.
//! Otherwise, you may experience errors similar to: "Perhaps two different
//! versions of crate `accesskit` are being used?"
//!
//! [accesskit_crate]: https://crates.io/crates/accesskit
//! [`Entity`]: bevy_ecs::entity::Entity
//!
//! <!--
//! note: multi-line footnotes need to be indented like this!
//!
//! please do not remove the indentation, or the second paragraph will display
//! at the end of the module docs, **before** the footnotes...
//! -->
//!
//! [^accesskit_node_confusion]: Some users were confused about `AccessKit`'s
//!  `Node` type, sometimes thinking it was Bevy UI's primary way to define
//!  nodes!
//!
//!     For this reason, its re-export was removed by default. Users who need
//!     its types can instead manually depend on the `accesskit` crate.

#[cfg(feature = "std")]
extern crate std;

extern crate alloc;

use alloc::{
    collections::{BTreeMap, BTreeSet},
    sync::Arc,
    vec::Vec,
};
use core::sync::atomic::{AtomicBool, Ordering};

use accesskit::{Affine, Node, NodeId, Tree, TreeId, TreeUpdate};
use bevy_app::Plugin;
use bevy_derive::{Deref, DerefMut};
use bevy_ecs::{
    component::Component, entity::Entity, message::Message, resource::Resource, schedule::SystemSet,
};

#[cfg(feature = "bevy_reflect")]
use {
    bevy_ecs::reflect::ReflectResource, bevy_reflect::std_traits::ReflectDefault,
    bevy_reflect::Reflect,
};

#[cfg(feature = "serialize")]
use serde::{Deserialize, Serialize};

#[cfg(all(feature = "bevy_reflect", feature = "serialize"))]
use bevy_reflect::{ReflectDeserialize, ReflectSerialize};

/// Wrapper struct for [`accesskit::ActionRequest`].
///
/// This newtype is required to use `ActionRequest` as a Bevy `Event`.
#[derive(Message, Deref, DerefMut)]
#[cfg_attr(feature = "serialize", derive(Serialize, Deserialize))]
pub struct ActionRequest(pub accesskit::ActionRequest);

/// Registers an entity as the stable graft for an independently produced
/// accessibility subtree.
///
/// The entity carrying this component is the provider identity and supplies
/// the graft node's ID in the window's root tree. `tree_id` identifies the
/// independent AccessKit namespace and must remain stable for the lifetime of
/// the attachment. Replacing this component starts a new attachment
/// generation, even when its values are unchanged. Tree IDs must be unique
/// among providers attached to the same window; independent native window
/// adapters may reuse the same toolkit-local tree ID.
#[derive(Component, Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessibilitySubtreeProvider {
    window: Entity,
    tree_id: TreeId,
}

impl AccessibilitySubtreeProvider {
    /// Creates a provider targeting `window` with a non-root AccessKit tree ID.
    pub fn new(window: Entity, tree_id: TreeId) -> Result<Self, AccessibilitySubtreeProviderError> {
        if tree_id == TreeId::ROOT {
            return Err(AccessibilitySubtreeProviderError::RootTreeId);
        }
        Ok(Self { window, tree_id })
    }

    /// Returns the Bevy window that owns the native accessibility adapter.
    pub const fn window(&self) -> Entity {
        self.window
    }

    /// Returns the stable AccessKit namespace assigned to this provider.
    pub const fn tree_id(&self) -> TreeId {
        self.tree_id
    }
}

/// Error returned while registering an accessibility subtree provider.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessibilitySubtreeProviderError {
    /// [`TreeId::ROOT`] is reserved for the Bevy-owned window tree.
    RootTreeId,
}

/// Total ordering key for sibling accessibility subtree grafts.
///
/// Equal values are ordered by the provider entity, making the final order
/// deterministic.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct AccessibilitySubtreeOrder(pub i64);

/// Transform applied at an accessibility subtree's graft node.
///
/// This is useful for embedding a child toolkit whose node bounds are local to
/// a viewport. The transform is inherited by the subtree and therefore does
/// not require rewriting provider-owned nodes.
#[derive(Component, Clone, Copy, Debug, PartialEq)]
pub struct AccessibilitySubtreeTransform(pub Affine);

/// Claims parent-tree keyboard focus for an accessibility subtree.
///
/// The subtree snapshot always has an internal fallback focus. This marker is
/// separate: it tells the Bevy-owned window tree to focus the provider's graft.
/// At most one provider may carry this marker for a given window.
#[derive(Component, Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct AccessibilitySubtreeParentFocus;

/// A complete, retained snapshot of a provider-owned accessibility tree.
///
/// Snapshots are not incremental. Keeping the latest complete snapshot in ECS
/// lets the native owner replay it after accessibility activation or adapter
/// recreation. `sequence` must increase within one provider attachment; this
/// allows late asynchronous publications to be rejected without rolling the
/// native tree backwards.
#[derive(Component, Clone, Debug, PartialEq)]
pub struct AccessibilitySubtreeSnapshot {
    sequence: u64,
    tree: Arc<Tree>,
    root: NodeId,
    nodes: Arc<[(NodeId, Node)]>,
    node_indices: Arc<BTreeMap<NodeId, usize>>,
    focus: NodeId,
}

impl AccessibilitySubtreeSnapshot {
    /// Converts a complete AccessKit update into a retained subtree snapshot.
    ///
    /// The update's `tree_id` is intentionally ignored: the registered
    /// provider supplies the external namespace. This allows an embedded
    /// toolkit to publish its local [`TreeId::ROOT`] tree without rewriting
    /// any node IDs or cross-node relations.
    pub fn try_from_full_update(
        sequence: u64,
        update: TreeUpdate,
    ) -> Result<Self, AccessibilitySubtreeSnapshotError> {
        let Some(tree) = update.tree else {
            return Err(AccessibilitySubtreeSnapshotError::MissingTree);
        };
        let mut node_indices = BTreeMap::new();
        for (index, (id, _)) in update.nodes.iter().enumerate() {
            if node_indices.insert(*id, index).is_some() {
                return Err(AccessibilitySubtreeSnapshotError::DuplicateNode(*id));
            }
        }
        if !node_indices.contains_key(&tree.root) {
            return Err(AccessibilitySubtreeSnapshotError::MissingRoot(tree.root));
        }
        if !node_indices.contains_key(&update.focus) {
            return Err(AccessibilitySubtreeSnapshotError::MissingFocus(
                update.focus,
            ));
        }
        let mut parent_counts = BTreeMap::new();
        for id in node_indices.keys() {
            parent_counts.insert(*id, 0usize);
        }
        let mut edges = BTreeSet::new();
        for (id, node) in &update.nodes {
            if node.tree_id().is_some() {
                return Err(AccessibilitySubtreeSnapshotError::NestedGraft(*id));
            }
            for child in node.children() {
                if !edges.insert((*id, *child)) {
                    return Err(AccessibilitySubtreeSnapshotError::DuplicateChild {
                        parent: *id,
                        child: *child,
                    });
                }
                let Some(parent_count) = parent_counts.get_mut(child) else {
                    return Err(AccessibilitySubtreeSnapshotError::MissingChild {
                        parent: *id,
                        child: *child,
                    });
                };
                if *child == tree.root {
                    return Err(AccessibilitySubtreeSnapshotError::RootHasParent(*id));
                }
                *parent_count += 1;
            }
        }
        for (id, parent_count) in &parent_counts {
            if *id == tree.root {
                continue;
            }
            match parent_count {
                0 => return Err(AccessibilitySubtreeSnapshotError::Disconnected(*id)),
                1 => {}
                _ => return Err(AccessibilitySubtreeSnapshotError::MultipleParents(*id)),
            }
        }
        let mut reachable = BTreeSet::new();
        let mut pending = Vec::with_capacity(update.nodes.len());
        pending.push(tree.root);
        while let Some(id) = pending.pop() {
            if !reachable.insert(id) {
                continue;
            }
            let node = &update.nodes[node_indices[&id]].1;
            pending.extend_from_slice(node.children());
        }
        if let Some((id, _)) = update.nodes.iter().find(|(id, _)| !reachable.contains(id)) {
            return Err(AccessibilitySubtreeSnapshotError::DisconnectedOrCyclic(*id));
        }
        Ok(Self {
            sequence,
            root: tree.root,
            tree: Arc::new(tree),
            nodes: update.nodes.into(),
            node_indices: Arc::new(node_indices),
            focus: update.focus,
        })
    }

    /// Returns the provider-assigned publication sequence.
    pub const fn sequence(&self) -> u64 {
        self.sequence
    }

    /// Returns the root node ID within the provider's namespace.
    pub const fn root(&self) -> NodeId {
        self.root
    }

    /// Returns the node focused within the provider's namespace.
    pub const fn focus(&self) -> NodeId {
        self.focus
    }

    /// Returns the retained node for `id`, if it belongs to this snapshot.
    pub fn node(&self, id: NodeId) -> Option<&Node> {
        self.node_indices
            .get(&id)
            .map(|index| &self.nodes[*index].1)
    }

    /// Returns whether `id` belongs to this snapshot and advertises `action`.
    pub fn node_supports_action(&self, id: NodeId, action: accesskit::Action) -> bool {
        self.node(id)
            .is_some_and(|node| node.supports_action(action))
    }

    /// Builds the complete AccessKit update for `tree_id`.
    pub fn tree_update(&self, tree_id: TreeId) -> TreeUpdate {
        TreeUpdate {
            nodes: self.nodes.to_vec(),
            tree: Some((*self.tree).clone()),
            tree_id,
            focus: self.focus,
        }
    }
}

/// Validation error for a retained accessibility subtree snapshot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessibilitySubtreeSnapshotError {
    /// A full snapshot must include [`Tree`] metadata.
    MissingTree,
    /// The declared root is absent from the node list.
    MissingRoot(NodeId),
    /// The declared focus is absent from the node list.
    MissingFocus(NodeId),
    /// A node ID occurs more than once.
    DuplicateNode(NodeId),
    /// A node attempts to graft another independent subtree.
    NestedGraft(NodeId),
    /// A parent contains the same child more than once.
    DuplicateChild {
        /// Parent containing the repeated entry.
        parent: NodeId,
        /// Repeated child.
        child: NodeId,
    },
    /// Some node lists the declared root as its child.
    RootHasParent(NodeId),
    /// A non-root node has no parent.
    Disconnected(NodeId),
    /// A node has more than one structural parent.
    MultipleParents(NodeId),
    /// A component cannot be reached from the declared root, including a
    /// disconnected cycle.
    DisconnectedOrCyclic(NodeId),
    /// A parent references a child absent from the complete snapshot.
    MissingChild {
        /// The node containing the invalid child reference.
        parent: NodeId,
        /// The missing child node.
        child: NodeId,
    },
}

/// Per-window native accessibility activation state.
///
/// Platform backends attach this component to their window entity. Clones
/// share the underlying atomic state so activation callbacks can update it
/// before the next ECS pass.
#[derive(Component, Clone, Debug, Default)]
pub struct WindowAccessibilityState(Arc<AtomicBool>);

impl WindowAccessibilityState {
    /// Returns whether the platform adapter is currently active.
    pub fn is_active(&self) -> bool {
        self.0.load(Ordering::SeqCst)
    }

    /// Updates the state from the platform accessibility owner.
    ///
    /// This backend seam is public only because platform integrations live in
    /// separate crates; applications should treat the component as read-only.
    #[doc(hidden)]
    pub fn set_platform_active(&self, active: bool) {
        self.0.store(active, Ordering::SeqCst);
    }
}

/// Identifies one native adapter lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccessibilityAdapterGeneration(pub u64);

/// Identifies one activation request within a native adapter lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccessibilityActivationGeneration(pub u64);

/// Identifies one provider attachment to a window.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct AccessibilityAttachmentGeneration(pub u64);

/// Identifies one provider attachment within a native accessibility lifetime.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct AccessibilitySubtreeLifecycleToken {
    /// Source window.
    pub window: Entity,
    /// Stable graft/provider entity.
    pub provider: Entity,
    /// Provider tree namespace.
    pub tree_id: TreeId,
    /// Native adapter lifetime.
    pub adapter_generation: AccessibilityAdapterGeneration,
    /// Activation request represented by this lifecycle edge.
    pub activation_generation: AccessibilityActivationGeneration,
    /// Attachment lifetime represented by this lifecycle edge.
    pub attachment_generation: AccessibilityAttachmentGeneration,
}

/// Ordered lifecycle edge for an accessibility subtree attachment.
///
/// Activation and deactivation share one Bevy message stream so rapid native
/// lifecycle transitions cannot be reordered by independent message cursors.
#[derive(Message, Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccessibilitySubtreeLifecycle {
    /// The attachment may build or publish its retained subtree.
    Activated(AccessibilitySubtreeLifecycleToken),
    /// The represented attachment lifetime is no longer native-live.
    Deactivated(AccessibilitySubtreeLifecycleToken),
}

/// A native accessibility action routed to a validated subtree attachment.
#[derive(Message, Clone, Debug, PartialEq)]
pub struct AccessibilitySubtreeActionRequest {
    /// Source window.
    pub window: Entity,
    /// Stable graft/provider entity.
    pub provider: Entity,
    /// Native adapter lifetime captured when the action arrived.
    pub adapter_generation: AccessibilityAdapterGeneration,
    /// Activation request captured when the action arrived.
    pub activation_generation: AccessibilityActivationGeneration,
    /// Attachment lifetime captured when the action arrived.
    pub attachment_generation: AccessibilityAttachmentGeneration,
    /// Snapshot sequence exposed to the native adapter when the action arrived.
    pub snapshot_sequence: u64,
    /// Parent-tree transform committed for this provider when the action
    /// arrived. This is captured at the native callback boundary so a later
    /// root-composition pass cannot reinterpret queued coordinates.
    pub committed_transform: Option<AccessibilitySubtreeTransform>,
    /// Provider-local AccessKit request. Its target tree is the provider's
    /// registered non-root tree ID.
    pub request: accesskit::ActionRequest,
}

/// Tracks whether an assistive technology has requested accessibility
/// information.
///
/// This type is a [`Resource`] initialized by the
/// [`AccessibilityPlugin`]. It may be useful if a third-party plugin needs to
/// conditionally integrate with `AccessKit`.
///
/// In other words, this resource represents whether accessibility providers
/// are "turned on" or "turned off" across an entire Bevy `App`.
///
/// By default, it is set to `false`, indicating that nothing has requested
/// accessibility information yet.
///
/// [`Resource`]: bevy_ecs::resource::Resource
#[derive(Resource, Default, Clone, Debug, Deref, DerefMut)]
#[cfg_attr(
    feature = "bevy_reflect",
    derive(Reflect),
    reflect(Default, Clone, Resource)
)]
pub struct AccessibilityRequested(Arc<AtomicBool>);

impl AccessibilityRequested {
    /// Checks if any assistive technology has requested accessibility
    /// information.
    ///
    /// If so, this method returns `true`, indicating that accessibility tree
    /// updates should be sent.
    pub fn get(&self) -> bool {
        self.load(Ordering::SeqCst)
    }

    /// Sets the app's preference for sending accessibility updates.
    ///
    /// If the `value` argument is `true`, this method requests that the app,
    /// including both Bevy and third-party interfaces, provides updates to
    /// accessibility information.
    ///
    /// Setting with `false` requests that the entire app stops providing these
    /// updates.
    pub fn set(&self, value: bool) {
        self.store(value, Ordering::SeqCst);
    }
}

/// Determines whether Bevy's ECS updates the accessibility tree.
///
/// This [`Resource`] tells Bevy internals whether it should be handling
/// `AccessKit` updates (`true`), or if something else is doing that (`false`).
///
/// It defaults to `true`. So, by default, Bevy is configured to maintain the
/// `AccessKit` tree.
///
/// Set to `false` in cases where an external GUI library is sending
/// accessibility updates instead. When this option is set inconsistently with
/// that requirement, the external library and ECS will generate conflicting
/// updates.
///
/// [`Resource`]: bevy_ecs::resource::Resource
#[derive(Resource, Clone, Debug, Deref, DerefMut)]
#[cfg_attr(feature = "serialize", derive(Serialize, Deserialize))]
#[cfg_attr(
    feature = "bevy_reflect",
    derive(Reflect),
    reflect(Resource, Clone, Default)
)]
#[cfg_attr(
    all(feature = "bevy_reflect", feature = "serialize"),
    reflect(Serialize, Deserialize)
)]
pub struct ManageAccessibilityUpdates(bool);

impl Default for ManageAccessibilityUpdates {
    fn default() -> Self {
        Self(true)
    }
}

impl ManageAccessibilityUpdates {
    /// Returns `true` if Bevy's ECS should update the accessibility tree.
    pub fn get(&self) -> bool {
        self.0
    }

    /// Sets whether Bevy's ECS should update the accessibility tree.
    pub fn set(&mut self, value: bool) {
        self.0 = value;
    }
}

/// Represents an entity to `AccessKit` through an [`accesskit::Node`].
///
/// Platform-specific accessibility APIs utilize `AccessKit` nodes in their
/// accessibility frameworks. So, this component acts as a translation between
/// "Bevy entity" and "platform-agnostic accessibility element".
///
/// ## Organization in the `AccessKit` Accessibility Tree
///
/// `AccessKit` allows users to form a "tree of nodes" providing accessibility
/// information. That tree is **not** Bevy's ECS!
///
/// To explain, let's say this component is added to an entity, `E`.
///
/// ### Parent and Child
///
/// If `E` has a parent, `P`, and `P` also has this `AccessibilityNode`
/// component, then `E`'s `AccessKit` node will be a child of `P`'s `AccessKit`
/// node.
///
/// Resulting `AccessKit` tree:
/// - P
///     - E
///
/// In other words, parent-child relationships are maintained, but only if both
/// have this component.
///
/// ### On the Window
///
/// If `E` doesn't have a parent, or if the immediate parent doesn't have an
/// `AccessibilityNode`, its `AccessKit` node will be an immediate child of the
/// primary window.
///
/// Resulting `AccessKit` tree:
/// - Primary window
///     - E
///
/// When there's no `AccessKit`-compatible parent, the child lacks hierarchical
/// information in `AccessKit`. As such, it is placed directly under the
/// primary window on the `AccessKit` tree.
///
/// This behavior may or may not be intended, so please utilize
/// `AccessibilityNode`s with care.
#[derive(Component, Clone, Deref, DerefMut, Default)]
#[cfg_attr(feature = "serialize", derive(Serialize, Deserialize))]
pub struct AccessibilityNode(
    /// A representation of this component's entity to `AccessKit`.
    ///
    /// Note that, with its parent struct acting as just a newtype, users are
    /// intended to directly update this field.
    pub Node,
);

impl From<Node> for AccessibilityNode {
    /// Converts an [`accesskit::Node`] into the Bevy Engine
    /// [`AccessibilityNode`] newtype.
    ///
    /// Doing so allows it to be inserted onto Bevy entities, representing Bevy
    /// entities in the `AccessKit` tree.
    fn from(node: Node) -> Self {
        Self(node)
    }
}

/// A system set relating to accessibility.
///
/// Helps run accessibility updates all at once.
#[derive(Debug, Hash, PartialEq, Eq, Clone, SystemSet)]
#[cfg_attr(feature = "serialize", derive(Serialize, Deserialize))]
#[cfg_attr(feature = "bevy_reflect", derive(Reflect))]
#[cfg_attr(
    all(feature = "bevy_reflect", feature = "serialize"),
    reflect(Serialize, Deserialize, Clone)
)]
pub enum AccessibilitySystems {
    /// Update the accessibility tree.
    Update,
    /// Route native actions after attachment changes have been committed.
    ///
    /// Native callbacks coalesce a Winit event-loop wake before these messages
    /// are drained. The backend schedules one consumer follow-up update for forwarded
    /// actions, so ordinary `Update` consumers cannot be stranded. Consumers
    /// that need same-update delivery may instead run after this set. If such
    /// a consumer publishes provider state after [`Self::Update`], the
    /// [`Self::RequestUpdate`] boundary schedules the composition follow-up.
    Actions,
    /// Request another update for late provider changes and native lifecycle
    /// edges that ordinary application `Update` systems have not yet observed.
    ///
    /// Winit also retires closed native adapters in this boundary before
    /// coalescing their single consumer follow-up request.
    RequestUpdate,
}

/// Plugin managing integration with accessibility APIs.
///
/// Note that it doesn't handle GUI aspects of this integration, instead
/// providing helpful resources for other interfaces to utilize.
///
/// ## Behavior
///
/// This plugin's main role is to initialize the [`AccessibilityRequested`] and
/// [`ManageAccessibilityUpdates`] resources to their default values, meaning:
///
/// - no assistive technologies have requested accessibility information yet,
///   and
/// - Bevy's ECS will manage updates to the accessibility tree.
#[derive(Default)]
pub struct AccessibilityPlugin;

impl Plugin for AccessibilityPlugin {
    fn build(&self, app: &mut bevy_app::App) {
        app.init_resource::<AccessibilityRequested>()
            .init_resource::<ManageAccessibilityUpdates>()
            .allow_ambiguous_component::<AccessibilityNode>()
            .allow_ambiguous_component::<AccessibilitySubtreeSnapshot>();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use accesskit::Role;
    use alloc::vec;

    fn full_update(tree_id: TreeId, root: NodeId, focus: NodeId) -> TreeUpdate {
        let mut root_node = Node::new(Role::Group);
        if focus != root {
            root_node.push_child(focus);
        }
        let mut nodes = vec![(root, root_node)];
        if focus != root {
            nodes.push((focus, Node::new(Role::Button)));
        }
        TreeUpdate {
            nodes,
            tree: Some(Tree::new(root)),
            tree_id,
            focus,
        }
    }

    #[test]
    fn provider_rejects_root_tree_id() {
        assert_eq!(
            AccessibilitySubtreeProvider::new(Entity::PLACEHOLDER, TreeId::ROOT),
            Err(AccessibilitySubtreeProviderError::RootTreeId)
        );
    }

    #[test]
    fn snapshot_retargets_complete_local_tree_without_rewriting_node_ids() {
        let local_root = NodeId(10);
        let local_focus = NodeId(11);
        let snapshot = AccessibilitySubtreeSnapshot::try_from_full_update(
            7,
            full_update(TreeId::ROOT, local_root, local_focus),
        )
        .unwrap();
        let external_tree = TreeId(accesskit::Uuid::from_u128(1));
        let update = snapshot.tree_update(external_tree);
        assert_eq!(update.tree_id, external_tree);
        assert_eq!(update.tree.unwrap().root, local_root);
        assert_eq!(update.focus, local_focus);
        assert_eq!(update.nodes[1].0, local_focus);
    }

    #[test]
    fn retained_snapshot_node_lookup_rejects_missing_and_unadvertised_actions() {
        let root = NodeId(10);
        let button = NodeId(11);
        let mut root_node = Node::new(Role::Group);
        root_node.push_child(button);
        let mut button_node = Node::new(Role::Button);
        button_node.add_action(accesskit::Action::Click);
        let snapshot = AccessibilitySubtreeSnapshot::try_from_full_update(
            1,
            TreeUpdate {
                nodes: vec![(root, root_node), (button, button_node)],
                tree: Some(Tree::new(root)),
                tree_id: TreeId::ROOT,
                focus: button,
            },
        )
        .unwrap();

        assert_eq!(snapshot.node(button).map(Node::role), Some(Role::Button));
        assert!(snapshot.node_supports_action(button, accesskit::Action::Click));
        assert!(!snapshot.node_supports_action(button, accesskit::Action::Focus));
        assert!(snapshot.node(NodeId(99)).is_none());
        assert!(!snapshot.node_supports_action(NodeId(99), accesskit::Action::Click));
    }

    #[test]
    fn snapshot_rejects_incremental_or_incomplete_updates() {
        let incremental = TreeUpdate {
            nodes: vec![(NodeId(1), Node::new(Role::Group))],
            tree: None,
            tree_id: TreeId::ROOT,
            focus: NodeId(1),
        };
        assert_eq!(
            AccessibilitySubtreeSnapshot::try_from_full_update(0, incremental),
            Err(AccessibilitySubtreeSnapshotError::MissingTree)
        );

        let mut root = Node::new(Role::Group);
        root.push_child(NodeId(2));
        let dangling = TreeUpdate {
            nodes: vec![(NodeId(1), root)],
            tree: Some(Tree::new(NodeId(1))),
            tree_id: TreeId::ROOT,
            focus: NodeId(1),
        };
        assert_eq!(
            AccessibilitySubtreeSnapshot::try_from_full_update(0, dangling),
            Err(AccessibilitySubtreeSnapshotError::MissingChild {
                parent: NodeId(1),
                child: NodeId(2),
            })
        );
    }

    #[test]
    fn full_snapshot_validator_rejects_every_non_tree_graph_shape_and_nested_graft() {
        let update = |nodes: Vec<(NodeId, Node)>| TreeUpdate {
            nodes,
            tree: Some(Tree::new(NodeId(1))),
            tree_id: TreeId::ROOT,
            focus: NodeId(1),
        };

        let disconnected = update(vec![
            (NodeId(1), Node::new(Role::Group)),
            (NodeId(2), Node::new(Role::Button)),
        ]);
        assert_eq!(
            AccessibilitySubtreeSnapshot::try_from_full_update(0, disconnected),
            Err(AccessibilitySubtreeSnapshotError::Disconnected(NodeId(2)))
        );

        let mut cycle_a = Node::new(Role::Group);
        cycle_a.push_child(NodeId(3));
        let mut cycle_b = Node::new(Role::Group);
        cycle_b.push_child(NodeId(2));
        let cycle = update(vec![
            (NodeId(1), Node::new(Role::Group)),
            (NodeId(2), cycle_a),
            (NodeId(3), cycle_b),
        ]);
        assert!(matches!(
            AccessibilitySubtreeSnapshot::try_from_full_update(0, cycle),
            Err(AccessibilitySubtreeSnapshotError::DisconnectedOrCyclic(_))
        ));

        let mut root = Node::new(Role::Group);
        root.set_children(vec![NodeId(2), NodeId(3)]);
        let mut first_parent = Node::new(Role::Group);
        first_parent.push_child(NodeId(4));
        let mut second_parent = Node::new(Role::Group);
        second_parent.push_child(NodeId(4));
        let multiple_parent = update(vec![
            (NodeId(1), root),
            (NodeId(2), first_parent),
            (NodeId(3), second_parent),
            (NodeId(4), Node::new(Role::Button)),
        ]);
        assert_eq!(
            AccessibilitySubtreeSnapshot::try_from_full_update(0, multiple_parent),
            Err(AccessibilitySubtreeSnapshotError::MultipleParents(NodeId(
                4
            )))
        );

        let mut duplicate_root = Node::new(Role::Group);
        duplicate_root.set_children(vec![NodeId(2), NodeId(2)]);
        let duplicate_child = update(vec![
            (NodeId(1), duplicate_root),
            (NodeId(2), Node::new(Role::Button)),
        ]);
        assert!(matches!(
            AccessibilitySubtreeSnapshot::try_from_full_update(0, duplicate_child),
            Err(AccessibilitySubtreeSnapshotError::DuplicateChild { .. })
        ));

        let mut child = Node::new(Role::Group);
        child.push_child(NodeId(1));
        let root_has_parent = update(vec![
            (NodeId(1), Node::new(Role::Group)),
            (NodeId(2), child),
        ]);
        assert_eq!(
            AccessibilitySubtreeSnapshot::try_from_full_update(0, root_has_parent),
            Err(AccessibilitySubtreeSnapshotError::RootHasParent(NodeId(2)))
        );

        let mut nested = Node::new(Role::Group);
        nested.set_tree_id(TreeId(accesskit::Uuid::from_u128(2)));
        let nested_graft = update(vec![(NodeId(1), nested)]);
        assert_eq!(
            AccessibilitySubtreeSnapshot::try_from_full_update(0, nested_graft),
            Err(AccessibilitySubtreeSnapshotError::NestedGraft(NodeId(1)))
        );
    }

    #[test]
    fn full_snapshot_validator_handles_large_linear_tree_without_quadratic_scans() {
        const NODE_COUNT: u64 = 8_192;
        let mut nodes = Vec::with_capacity(NODE_COUNT as usize);
        for raw in 1..=NODE_COUNT {
            let mut node = Node::new(Role::Group);
            if raw < NODE_COUNT {
                node.push_child(NodeId(raw + 1));
            }
            nodes.push((NodeId(raw), node));
        }
        let snapshot = AccessibilitySubtreeSnapshot::try_from_full_update(
            1,
            TreeUpdate {
                nodes,
                tree: Some(Tree::new(NodeId(1))),
                tree_id: TreeId::ROOT,
                focus: NodeId(NODE_COUNT),
            },
        )
        .unwrap();
        assert_eq!(snapshot.root(), NodeId(1));
        assert_eq!(snapshot.focus(), NodeId(NODE_COUNT));
    }
}
