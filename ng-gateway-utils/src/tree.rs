use std::{
    cmp::Ordering,
    collections::{HashMap, HashSet},
    hash::Hash,
};

/// Trait for tree operations
pub trait TreeNode: Sized {
    /// ID type used for node identification
    type Id: Eq + Hash + Clone;

    /// Get node's unique ID
    fn id(&self) -> Self::Id;

    /// Get node's parent ID
    fn parent_id(&self) -> Self::Id;

    /// Get immutable reference to children
    fn children(&self) -> &[Self];

    /// Get mutable reference to children
    fn children_mut(&mut self) -> &mut Vec<Self>;

    /// Check if node has children
    fn has_children(&self) -> bool;

    /// Get mutable reference to has_children flag
    fn has_children_mut(&mut self) -> &mut bool;

    /// Get sort key for sorting
    fn sort_key(&self) -> i32;

    /// Compare this node with another for sorting
    /// This method should implement a multi-level comparison strategy
    /// Default implementation compares by id only
    fn compare(&self, other: &Self) -> Ordering {
        self.sort_key().cmp(&other.sort_key())
    }
}

/// Build a tree or forest structure from any nodes that can be converted to TreeNode
///
/// # Arguments
///
/// * `nodes` - Vector of nodes convertible to tree nodes
/// * `root_id` - Optional ID of the root node:
///   - If Some(id), builds a tree with nodes having parent_id = id as top level
///   - If None, builds a forest with nodes having no parent in the dataset as top level
///
/// # Returns
///
/// A vector of tree nodes representing the top-level nodes, sorted by their compare method
pub fn build_tree<N, T>(nodes: Vec<N>, root_id: Option<T::Id>) -> Vec<T>
where
    T: TreeNode + Clone,
    N: Into<T>,
    T::Id: Ord, // Needed for default compare implementation
{
    if nodes.is_empty() {
        return Vec::new();
    }

    // Maps for efficient lookups
    let mut children_map: HashMap<T::Id, Vec<T::Id>> = HashMap::with_capacity(nodes.len());
    let mut id_to_node: HashMap<T::Id, T> = HashMap::with_capacity(nodes.len());
    let mut all_ids = HashSet::with_capacity(nodes.len());

    // First pass: Process all nodes
    for node in nodes {
        let tree_node = node.into();
        let id = tree_node.id();
        let parent_id = tree_node.parent_id();

        all_ids.insert(id.clone());

        // Store node by ID
        id_to_node.insert(id.clone(), tree_node);

        // Group nodes by parent ID
        children_map
            .entry(parent_id)
            .or_insert_with(Vec::new)
            .push(id);
    }

    // Sort all child lists using the compare method
    for (_, child_ids) in children_map.iter_mut() {
        child_ids.sort_by(|a, b| {
            let node_a = id_to_node.get(a).unwrap();
            let node_b = id_to_node.get(b).unwrap();
            node_a.compare(node_b)
        });
    }

    match root_id {
        // If root_id is provided, build a tree with specified root
        Some(root) => children_map
            .get(&root)
            .map(|root_ids| {
                root_ids
                    .iter()
                    .map(|id| build_subtree(id.clone(), &id_to_node, &children_map))
                    .collect()
            })
            .unwrap_or_default(),

        // If no root_id, build a forest (find nodes whose parents are not in our dataset)
        None => {
            let mut top_level_nodes: Vec<T::Id> = id_to_node
                .iter()
                .filter_map(|(id, node)| {
                    if !all_ids.contains(&node.parent_id()) {
                        Some(id.clone())
                    } else {
                        None
                    }
                })
                .collect();

            // Sort top-level nodes using the compare method
            top_level_nodes.sort_by(|a, b| {
                let node_a = id_to_node.get(a).unwrap();
                let node_b = id_to_node.get(b).unwrap();
                node_a.compare(node_b)
            });

            top_level_nodes
                .into_iter()
                .map(|id| build_subtree(id, &id_to_node, &children_map))
                .collect()
        }
    }
}

// Helper function for recursive tree building
fn build_subtree<T>(
    id: T::Id,
    id_to_node: &HashMap<T::Id, T>,
    children_map: &HashMap<T::Id, Vec<T::Id>>,
) -> T
where
    T: TreeNode + Clone,
{
    let mut node = id_to_node.get(&id).unwrap().clone();

    if let Some(child_ids) = children_map.get(&id) {
        if !child_ids.is_empty() {
            let children = child_ids
                .iter()
                .map(|child_id| build_subtree(child_id.clone(), id_to_node, children_map))
                .collect();

            *node.children_mut() = children;
            *node.has_children_mut() = true;
        }
    }

    node
}
