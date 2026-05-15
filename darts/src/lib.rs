//! Rust port of Darts-clone (http://chasen.org/~taku/software/darts/)
//!
//! This implementation provides read-only access and a builder
//! for creating Darts arrays.

use std::mem;

/// A single unit in the Double-Array.
///
/// This is a transparent wrapper around a `u32` to replicate the
/// exact bit layout from the original `darts.h`.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
struct DoubleArrayUnit(u32);

impl DoubleArrayUnit {
    /// Mask to extract the value from a Leaf/Value node. (bits 0-30)
    const VALUE_MASK: u32 = (1 << 31) - 1; // 0x7FFFFFFF
    /// Flag indicating a Leaf/Value node.
    const LEAF_FLAG: u32 = 1 << 31; // 0x80000000
    /// Flag indicating a transition node has an associated leaf.
    const HAS_LEAF_FLAG: u32 = 1 << 8; // 0x00000100
    /// Flag for 'large offset' encoding.
    const OFFSET_FLAG: u32 = 1 << 9; // 0x00000200
    /// Mask to extract the label from a Transition node.
    const LABEL_MASK: u32 = 0xFF; // 0x000000FF

    /// C++: `bool has_leaf() const`
    /// Checks if a transition node is also the end of a key.
    #[inline]
    fn has_leaf(&self) -> bool {
        (self.0 & Self::HAS_LEAF_FLAG) != 0
    }

    /// C++: `value_type value() const`
    /// Gets the `i32` value from a Leaf/Value node.
    #[inline]
    fn value(&self) -> i32 {
        // The value is stored in bits 0-30.
        (self.0 & Self::VALUE_MASK) as i32
    }

    /// C++: `id_type label() const`
    /// Gets the label from a transition node.
    #[inline]
    fn label(&self) -> u32 {
        self.0 & (Self::LEAF_FLAG | Self::LABEL_MASK)
    }

    /// C++: `id_type offset() const`
    /// Gets the offset from a transition node.
    /// This decodes the complex offset encoding from `darts.h`.
    #[inline]
    fn offset(&self) -> u32 {
        let shift = (self.0 & Self::OFFSET_FLAG) >> 6; // 0 or 8
        (self.0 >> 10) << shift
    }
}

/// A read-only view into a Darts Double-Array.
///
/// The lifetime `'a` refers to the byte slice holding the array data.
pub struct DoubleArray<'a> {
    array: &'a [DoubleArrayUnit],
}

#[allow(unused)]
/// Represents a single search result.
/// C++: `result_pair_type`
#[derive(Debug, Clone, Copy, Default)]
pub struct Match {
    /// C++: `value_type value` (an `i32` in `gram_db`)
    pub value: i32,
    /// C++: `std::size_t length` (length of the matched key)
    pub length: usize,
}

impl<'a> DoubleArray<'a> {
    /// Creates a new `DoubleArray` view from a raw byte slice.
    pub fn new(data: &'a [u8]) -> Result<Self, &'static str> {
        // Ensure data length is a multiple of 4 (size of u32).
        if data.len() % mem::size_of::<u32>() != 0 {
            return Err("Darts data length is not a multiple of 4");
        }

        // Check alignment. This is critical.
        let align = mem::align_of::<u32>();
        if data.as_ptr() as usize % align != 0 {
            // This can happen if the array_data_start_pos in gram_db
            // is not a multiple of 4.
            return Err("Darts array data is not aligned to 4 bytes");
        }

        // `unsafe` is required to cast the byte slice to a u32 slice.
        let array_u32: &'a [u32] = unsafe {
            let len = data.len() / mem::size_of::<u32>();
            let ptr = data.as_ptr() as *const u32;
            std::slice::from_raw_parts(ptr, len)
        };

        // We can safely cast to `&'a [DoubleArrayUnit]`
        // because `DoubleArrayUnit` is `#[repr(transparent)]`.
        let array = unsafe {
            std::mem::transmute::<&'a [u32], &'a [DoubleArrayUnit]>(array_u32)
        };

        if array.is_empty() {
            return Err("Darts array is empty");
        }
        Ok(Self { array })
    }

    /// C++: `value_type traverse(...)`
    ///
    /// Follows a key through the trie, updating `node_pos` and `key_pos`.
    ///
    /// The `key` parameter now accepts any type that can be referenced as a byte slice.
    pub fn traverse<K: AsRef<[u8]>>(
        &self,
        key: K,
        node_pos: &mut usize,
        key_pos: &mut usize,
    ) -> i32 {
        let key_bytes = key.as_ref();
        let mut id = *node_pos as u32;

        if id >= self.array.len() as u32 {
            return -2; // Invalid start node
        }
        let mut unit = self.array[id as usize];

        while *key_pos < key_bytes.len() {
            let c = key_bytes[*key_pos];
            let next_id = id ^ unit.offset() ^ (c as u32);

            if next_id >= self.array.len() as u32 {
                return -2; // Mismatch (out of bounds)
            }

            let next_unit = self.array[next_id as usize];

            // C++: if (unit.label() != static_cast<uchar_type>(key[key_pos]))
            if next_unit.label() != (c as u32) {
                return -2; // Mismatch (label check)
            }

            // Advance
            id = next_id;
            unit = next_unit;
            *node_pos = id as usize;
            *key_pos += 1;
        }

        // Loop finished. Check for leaf at the current node.
        if !unit.has_leaf() {
            return -1; // Ended on a non-leaf node
        }

        // Get the leaf node
        let leaf_node_id = id ^ unit.offset();
        if leaf_node_id >= self.array.len() as u32 {
            return -1; // Leaf points out of bounds, treat as non-leaf
        }
        let leaf_unit = self.array[leaf_node_id as usize];

        // C++ `value()` returns i32
        leaf_unit.value()
    }

    /// C++: `std::size_t commonPrefixSearch(...)`
    ///
    /// Finds all keys in the trie that are prefixes of the given `key`.
    ///
    /// The `key` parameter now accepts any type that can be referenced as a byte slice.
    pub fn common_prefix_search<K: AsRef<[u8]>>(
        &self,
        key: K,
        node_pos: usize,
        results: &mut [Match],
    ) -> usize {
        let key_bytes = key.as_ref();
        let mut num_results = 0;
        if node_pos >= self.array.len() {
            return 0;
        }

        let mut unit = self.array[node_pos];
        let mut id = node_pos as u32;

        // C++: node_pos ^= unit.offset();
        // Get the base offset for transitions from this node.
        id ^= unit.offset();

        for (i, &c) in key_bytes.iter().enumerate() {
            // C++: node_pos ^= static_cast<uchar_type>(key[length]);
            let next_id = id ^ (c as u32);

            if next_id >= self.array.len() as u32 {
                return num_results; // Out of bounds
            }

            // C++: unit = array_[node_pos];
            unit = self.array[next_id as usize];

            // C++: if (unit.label() != static_cast<uchar_type>(key[length]))
            if unit.label() != (c as u32) {
                return num_results; // Label mismatch, stop search
            }

            // C++: node_pos ^= unit.offset();
            // Get the base offset for the *next* set of transitions.
            id = next_id ^ unit.offset();

            // C++: if (unit.has_leaf())
            if unit.has_leaf() {
                // This node is a key. `id` now points to the leaf/value node.
                if id >= self.array.len() as u32 {
                    // Should not happen, but good to check.
                    continue;
                }
                let leaf_unit = self.array[id as usize];
                let value = leaf_unit.value();
                let length = i + 1; // 0-indexed i -> 1-based length

                if num_results < results.len() {
                    results[num_results] = Match { value, length };
                }
                num_results += 1;
            }
        }

        num_results
    }

    /// Finds all valid child transitions from a given node.
    /// Returns a Vec of (character_label, child_node_index).
    fn get_children(&self, node_pos: usize) -> Vec<(u8, usize)> {
        let mut children = Vec::new();
        if node_pos >= self.array.len() {
            return children;
        }
        let unit = self.array[node_pos];

        // This is the base offset for all transitions from this node
        let base = (node_pos as u32) ^ unit.offset();

        // Iterate through all possible byte values
        for c_u32 in 0..=255 {
            let c = c_u32 as u8;
            let next_id = base ^ c_u32;

            if next_id >= self.array.len() as u32 {
                continue; // Transition is out of bounds
            }

            let next_unit = self.array[next_id as usize];

            // Check if the label matches. This confirms a valid transition.
            if next_unit.label() == c_u32 {
                // This is a valid child transition
                children.push((c, next_id as usize));
            }
        }
        children
    }

    /// Recursively lists all keys from a given node.
    fn list_keys_recursive(
        &self,
        node_pos: usize,
        prefix: &mut Vec<u8>,
        results: &mut Vec<(Vec<u8>, i32)>,
    ) {
        if node_pos >= self.array.len() {
            return;
        }

        // Check if the current node is a leaf
        let unit = self.array[node_pos];
        if unit.has_leaf() {
            let leaf_node_id = (node_pos as u32) ^ unit.offset();
            if (leaf_node_id as usize) < self.array.len() {
                let leaf_unit = self.array[leaf_node_id as usize];
                // Add the current prefix as a found key
                results.push((prefix.clone(), leaf_unit.value()));
            }
        }

        // Recurse for all children
        for (c, child_node_id) in self.get_children(node_pos) {
            prefix.push(c);
            self.list_keys_recursive(child_node_id, prefix, results);
            prefix.pop(); // Backtrack
        }
    }

    /// Lists all keys and values stored in the trie.
    pub fn list_all_keys(&self) -> Vec<(Vec<u8>, i32)> {
        let mut results = Vec::new();
        let mut prefix = Vec::new();
        // Start traversal from the root node (index 0)
        self.list_keys_recursive(0, &mut prefix, &mut results);
        results
    }
}


// Type aliases for clarity, matching C++ version
type Id = u32;
type Value = i32;

// Custom error type for the build process
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BuildError {
    Message(&'static str),
}

impl From<&'static str> for BuildError {
    fn from(s: &'static str) -> Self {
        BuildError::Message(s)
    }
}


/// C++: `DoubleArrayBuilderUnit`
/// This is a *mutable* unit used during the build process.
/// It has a different API than the read-only `DoubleArrayUnit`.
#[repr(transparent)]
#[derive(Clone, Copy, Debug, Default)]
struct BuildUnit(u32);

impl BuildUnit {
    fn set_has_leaf(&mut self, has_leaf: bool) {
        if has_leaf {
            self.0 |= 1 << 8; // HAS_LEAF_FLAG
        } else {
            self.0 &= !(1 << 8);
        }
    }

    fn set_value(&mut self, value: Value) {
        self.0 = (value as u32) | (1 << 31); // LEAF_FLAG
    }

    fn set_label(&mut self, label: u8) {
        self.0 = (self.0 & !0xFF) | (label as u32);
    }

    fn set_offset(&mut self, offset: Id) -> Result<(), BuildError> {
        if offset >= 1 << 29 {
            return Err(BuildError::from("Failed to modify unit: too large offset"));
        }
        // Mask to keep LEAF, HAS_LEAF, LABEL bits
        self.0 &= DoubleArrayUnit::LEAF_FLAG | DoubleArrayUnit::HAS_LEAF_FLAG | DoubleArrayUnit::LABEL_MASK; 
        
        if offset < 1 << 21 {
            self.0 |= offset << 10;
        } else {
            self.0 |= (offset << 2) | DoubleArrayUnit::OFFSET_FLAG; // OFFSET_FLAG
        }
        Ok(())
    }
}

/// C++: `DoubleArrayBuilderExtraUnit`
/// Stores the free-list links (prev/next) and state (is_fixed, is_used)
/// for each unit in the builder.
#[derive(Clone, Copy, Debug, Default)]
struct BuildExtra {
    prev: Id,
    next: Id,
    is_fixed: bool,
    is_used: bool,
}

/// C++: `Keyset`
/// A simple wrapper to abstract access to keys and (optional) values.
pub struct Keyset<'a> {
    keys: &'a [&'a [u8]],
    values: Option<&'a [Value]>,
}

impl<'a> Keyset<'a> {
    fn num_keys(&self) -> usize {
        self.keys.len()
    }
    fn key(&self, id: usize) -> &'a [u8] {
        self.keys[id]
    }
    /// Gets the byte at `depth` for `key_id`, or 0 if out of bounds.
    fn key_byte(&self, key_id: usize, depth: usize) -> u8 {
        self.keys[key_id].get(depth).cloned().unwrap_or(0)
    }
    fn key_len(&self, id: usize) -> usize {
        self.keys[id].len()
    }
    fn has_values(&self) -> bool {
        self.values.is_some()
    }
    fn value(&self, id: usize) -> Value {
        self.values.map_or(id as Value, |v| v[id])
    }
}

// --- DAWG Builder (for keys with values) ---

/// C++: `DawgNode`
#[derive(Clone, Copy, Debug, Default)]
struct DawgNode {
    child: Id,
    sibling: Id,
    label: u8,
    is_state: bool,
    has_sibling: bool,
}

impl DawgNode {
    #[allow(unused)]
    fn value(&self) -> Value {
        self.child as Value
    }
    fn set_value(&mut self, value: Value) {
        self.child = value as Id;
    }
    fn unit(&self) -> u32 {
        if self.label == 0 {
            (self.child << 1) | (if self.has_sibling { 1 } else { 0 })
        } else {
            (self.child << 2)
                | (if self.is_state { 2 } else { 0 })
                | (if self.has_sibling { 1 } else { 0 })
        }
    }
}

/// C++: `DawgUnit` (read-only view)
#[derive(Clone, Copy)]
struct DawgUnit(u32);
impl DawgUnit {
    fn child(&self) -> Id {
        self.0 >> 2
    }
    fn has_sibling(&self) -> bool {
        (self.0 & 1) == 1
    }
    fn value(&self) -> Value {
        (self.0 >> 1) as Value
    }
    fn is_state(&self) -> bool {
        (self.0 & 2) == 2
    }
}

/// C++: `BitVector`
struct BitVector {
    units: Vec<u32>,
    ranks: Box<[Id]>,
    num_ones: usize,
    size: usize,
}

impl BitVector {
    const UNIT_SIZE: usize = 32;

    fn new() -> Self {
        Self {
            units: Vec::new(),
            ranks: Box::new([]),
            num_ones: 0,
            size: 0,
        }
    }

    fn get(&self, id: usize) -> bool {
        let unit_idx = id / Self::UNIT_SIZE;
        let bit_idx = id % Self::UNIT_SIZE;
        (self.units[unit_idx] & (1 << bit_idx)) != 0
    }

    fn set(&mut self, id: usize, bit: bool) {
        if id >= self.size {
            return;
        }
        if bit {
            self.units[id / Self::UNIT_SIZE] |= 1 << (id % Self::UNIT_SIZE);
        } else {
            self.units[id / Self::UNIT_SIZE] &= !(1 << (id % Self::UNIT_SIZE));
        }
    }

    fn rank(&self, id: usize) -> Id {
        if id == 0 {
            return 0;
        }
        let id = id - 1; // 0-based index for rank

        let unit_id = id / Self::UNIT_SIZE;
        let mut rank = self.ranks[unit_id];

        let bits_to_check = id % Self::UNIT_SIZE;
        if bits_to_check > 0 {
            rank += (self.units[unit_id] & ((1u32.wrapping_shl(bits_to_check as u32)) - 1))
                .count_ones() as Id;
        }
        rank
    }

    fn append(&mut self) {
        if (self.size % Self::UNIT_SIZE) == 0 {
            self.units.push(0);
        }
        self.size += 1;
    }

    fn build(&mut self) {
        let mut ranks = vec![0; self.units.len()];
        let mut num_ones = 0;
        for (i, &unit) in self.units.iter().enumerate() {
            ranks[i] = num_ones as Id;
            num_ones += unit.count_ones() as usize;
        }
        self.ranks = ranks.into_boxed_slice();
        self.num_ones = num_ones;
    }
}

/// C++: `DawgBuilder`
struct DawgBuilder {
    nodes: Vec<DawgNode>,
    units: Vec<DawgUnit>,
    labels: Vec<u8>,
    is_intersections: BitVector,
    table: Vec<Id>,
    node_stack: Vec<Id>,
    recycle_bin: Vec<Id>,
    num_states: usize,
}

impl DawgBuilder {
    const INITIAL_TABLE_SIZE: usize = 1 << 10;

    fn new() -> Self {
        Self {
            nodes: Vec::new(),
            units: Vec::new(),
            labels: Vec::new(),
            is_intersections: BitVector::new(),
            table: Vec::new(),
            node_stack: Vec::new(),
            recycle_bin: Vec::new(),
            num_states: 0,
        }
    }

    fn init(&mut self) {
        self.table = vec![0; Self::INITIAL_TABLE_SIZE];
        self.append_node(); // root node (id 0)
        self.append_unit(); // dummy unit (id 0)
        self.num_states = 1;
        self.nodes[0].label = 0xFF; // sentinel
        self.node_stack.push(0);
    }

    fn finish(&mut self) {
        self.flush(0);
        self.units[0] = DawgUnit(self.nodes[0].unit());
        self.labels[0] = self.nodes[0].label;
        self.nodes.clear();
        self.table.clear();
        self.node_stack.clear();
        self.recycle_bin.clear();
        self.is_intersections.build();
    }

    fn insert(&mut self, key: &[u8], value: Value) -> Result<(), BuildError> {
        if value < 0 {
            return Err(BuildError::from("Failed to insert key: negative value"));
        }
        if key.is_empty() {
            return Err(BuildError::from("Failed to insert key: zero-length key"));
        }

        let mut id = 0;
        let mut key_pos = 0;

        // Iterate over key bytes + null terminator (0)
        for &key_label in key.iter().chain(std::iter::once(&0)) {
            let child_id = self.nodes[id as usize].child;
            if child_id == 0 {
                break;
            }

            // Check for null char in middle of key
            if key_pos < key.len() && key_label == 0 {
                return Err(BuildError::from("Failed to insert key: invalid null character"));
            }

            let unit_label = self.nodes[child_id as usize].label;
            if key_label < unit_label {
                return Err(BuildError::from("Failed to insert key: wrong key order"));
            } else if key_label > unit_label {
                self.nodes[child_id as usize].has_sibling = true;
                self.flush(child_id);
                break;
            }
            id = child_id;
            key_pos += 1;
        }

        if key_pos > key.len() {
            // If key_pos > key.len(), it means we consumed the null terminator,
            // so the key already existed.
            return Ok(()); // Duplicate key
        }

        // Append remaining suffix
        for &key_label in key[key_pos..].iter().chain(std::iter::once(&0)) {
            let child_id = self.append_node();
            if self.nodes[id as usize].child == 0 {
                self.nodes[child_id as usize].is_state = true;
            }
            self.nodes[child_id as usize].sibling = self.nodes[id as usize].child;
            self.nodes[child_id as usize].label = key_label;
            self.nodes[id as usize].child = child_id;
            self.node_stack.push(child_id);
            id = child_id;
        }
        self.nodes[id as usize].set_value(value);
        Ok(())
    }

    fn flush(&mut self, id: Id) {
        while *self.node_stack.last().unwrap() != id {
            let node_id = self.node_stack.pop().unwrap();

            // Check for table expansion
            if self.num_states >= self.table.len() - (self.table.len() >> 2) {
                self.expand_table();
            }

            let (hash_id, match_id) = self.find_node(node_id);
            let final_match_id = if match_id != 0 {
                // Node is already in table (intersection found)
                self.is_intersections.set(match_id as usize, true);
                match_id
            } else {
                // Node is new, convert to unit chain and store
                let mut unit_id = 0;
                let mut sibling_id = node_id;
                let mut num_siblings = 0;
                while sibling_id != 0 {
                    unit_id = self.append_unit(); // unit_id is the last appended unit ID
                    sibling_id = self.nodes[sibling_id as usize].sibling;
                    num_siblings += 1;
                }
                
                // The first unit of the chain is `unit_id - num_siblings + 1`
                let first_unit_id = unit_id - num_siblings + 1;

                // Write node data into the new unit chain backwards (C++ implementation detail)
                sibling_id = node_id;
                let mut current_unit_id = unit_id;
                while sibling_id != 0 {
                    self.units[current_unit_id as usize] =
                        DawgUnit(self.nodes[sibling_id as usize].unit());
                    self.labels[current_unit_id as usize] = self.nodes[sibling_id as usize].label;
                    sibling_id = self.nodes[sibling_id as usize].sibling;
                    if sibling_id != 0 {
                            current_unit_id -= 1; 
                    }
                }

                self.table[hash_id] = first_unit_id;
                self.num_states += 1;
                first_unit_id
            };

            // Free the temporary nodes
            let mut next_id = node_id;
            while next_id != 0 {
                let current_id = next_id;
                next_id = self.nodes[current_id as usize].sibling;
                self.free_node(current_id);
            }

            // Update parent's child pointer
            self.nodes[*(self.node_stack.last().unwrap()) as usize].child = final_match_id;
        }
        self.node_stack.pop();
    }

fn expand_table(&mut self) {
        let table_size = self.table.len() << 1;
        self.table = vec![0; table_size];

        for i in 1..self.units.len() {
            // FIX: The condition was inverted in the original port.
            // We should only index units that are leaves (label == 0) 
            // OR are marked as states.
            if self.labels[i] == 0 || self.units[i].is_state() {
                let (hash_id, _) = self.find_unit(i as Id);
                // If the table is full (which shouldn't happen with correct logic), 
                // find_unit returns an occupied slot or 0. 
                // We only write if we found a valid slot or to overwrite (logic assumes empty table).
                self.table[hash_id] = i as Id;
            }
        }
    }

    // Find an empty slot in the hash table for a unit chain starting at 'id'.
    fn find_unit(&self, id: Id) -> (usize, Id) {
        let len = self.table.len();
        let start_hash_id = self.hash_unit(id) as usize % len;
        let mut hash_id = start_hash_id;
        
        loop {
            let unit_id = self.table[hash_id];
            if unit_id == 0 {
                break;
            }
            hash_id = (hash_id + 1) % len;
            
            // Safety check: Stop if we have wrapped around to the start
            if hash_id == start_hash_id {
                break;
            }
        }
        (hash_id, 0) 
    }

    // Find a matching unit chain in the hash table for the node chain starting at 'node_id'.
    fn find_node(&self, node_id: Id) -> (usize, Id) {
        let len = self.table.len();
        let start_hash_id = self.hash_node(node_id) as usize % len;
        let mut hash_id = start_hash_id;

        loop {
            let unit_id = self.table[hash_id];
            if unit_id == 0 {
                break;
            }
            if self.are_equal(node_id, unit_id) {
                return (hash_id, unit_id); // Match found
            }
            hash_id = (hash_id + 1) % len;

            // Safety check: Stop if we have wrapped around to the start
            if hash_id == start_hash_id {
                break;
            }
        }
        (hash_id, 0) // No match found
    }

    fn are_equal(&self, mut node_id: Id, mut unit_id: Id) -> bool {
        // Find the start of the unit chain
        let mut node_count = 0;
        let mut current_node = node_id;
        while current_node != 0 {
            node_count += 1;
            current_node = self.nodes[current_node as usize].sibling;
        }

        let mut unit_count = 0;
        let mut current_unit = unit_id;
        while current_unit != 0 {
            unit_count += 1;
            if !self.units[current_unit as usize].has_sibling() {
                break;
            }
            current_unit += 1;
        }
        
        // Check if lengths match
        if node_count != unit_count {
            return false;
        }

        // Iterate and compare: node_id (forward) against unit_id (forward)
        let unit_id_end = unit_id + unit_count;
        while node_id != 0 {
            if unit_id >= unit_id_end { // Should not happen if lengths match
                return false;
            }
            
            if self.nodes[node_id as usize].unit() != self.units[unit_id as usize].0
                || self.nodes[node_id as usize].label != self.labels[unit_id as usize]
            {
                return false;
            }
            node_id = self.nodes[node_id as usize].sibling;
            unit_id += 1;
        }
        true
    }

    fn hash_unit(&self, mut id: Id) -> u32 {
        let mut hash_value = 0;
        while id != 0 {
            let unit = self.units[id as usize].0;
            let label = self.labels[id as usize];
            hash_value ^= Self::hash((label as u32) << 24 ^ unit);
            if !self.units[id as usize].has_sibling() {
                break;
            }
            id += 1;
        }
        hash_value
    }

    fn hash_node(&self, mut id: Id) -> u32 {
        let mut hash_value = 0;
        while id != 0 {
            let unit = self.nodes[id as usize].unit();
            let label = self.nodes[id as usize].label;
            hash_value ^= Self::hash((label as u32) << 24 ^ unit);
            id = self.nodes[id as usize].sibling;
        }
        hash_value
    }

    fn append_unit(&mut self) -> Id {
        self.is_intersections.append();
        self.units.push(DawgUnit(0));
        self.labels.push(0);
        (self.is_intersections.size - 1) as Id
    }

    fn append_node(&mut self) -> Id {
        if let Some(id) = self.recycle_bin.pop() {
            self.nodes[id as usize] = DawgNode::default();
            id
        } else {
            let id = self.nodes.len() as Id;
            self.nodes.push(DawgNode::default());
            id
        }
    }

    fn free_node(&mut self, id: Id) {
        self.recycle_bin.push(id);
    }

    fn hash(key: u32) -> u32 {
        let mut key = key;
        key = !key.wrapping_add(key << 15);
        key ^= key >> 12;
        key = key.wrapping_add(key << 2);
        key ^= key >> 4;
        key = key.wrapping_mul(2057);
        key ^= key >> 16;
        key
    }

    // --- Read-only accessors for DoubleArrayBuilder ---
    fn root(&self) -> Id { 0 }
    fn child(&self, id: Id) -> Id { self.units[id as usize].child() }
    fn sibling(&self, id: Id) -> Id { if self.units[id as usize].has_sibling() { id + 1 } else { 0 } }
    fn value(&self, id: Id) -> Value { self.units[id as usize].value() }
    fn is_leaf(&self, id: Id) -> bool { self.label(id) == 0 }
    fn label(&self, id: Id) -> u8 { self.labels[id as usize] }
    fn is_intersection(&self, id: Id) -> bool { id != 0 && self.is_intersections.get(id as usize) }
    fn intersection_id(&self, id: Id) -> Id { self.is_intersections.rank(id as usize) }
    fn num_intersections(&self) -> usize { self.is_intersections.num_ones }

}

// --- Main Double Array Builder ---

/// C++: `DoubleArrayBuilder`
/// This is the main builder class that converts a Keyset or DAWG
/// into a double-array.
pub struct DoubleArrayBuilder<'a> {
    progress_func: Option<fn(usize, usize)>,
    units: Vec<BuildUnit>,
    extras: Box<[BuildExtra]>,
    labels: Vec<u8>,
    /// For DAWG-based build: maps intersection_id to offset
    table: Box<[Id]>,
    extras_head: Id,

    keyset: Keyset<'a>,
    dawg: DawgBuilder,
}

// C++: BLOCK_SIZE = 256
const BLOCK_SIZE: usize = 256;
// C++: NUM_EXTRA_BLOCKS = 16
const NUM_EXTRA_BLOCKS: usize = 16;
// C++: NUM_EXTRAS = BLOCK_SIZE * NUM_EXTRA_BLOCKS
const NUM_EXTRAS: usize = BLOCK_SIZE * NUM_EXTRA_BLOCKS;

const UPPER_MASK: u32 = 0xFF << 21;
const LOWER_MASK: u32 = 0xFF;

impl<'a> DoubleArrayBuilder<'a> {
    fn new(
        keyset: Keyset<'a>,
        progress_func: Option<fn(usize, usize)>,
    ) -> Self {
        Self {
            progress_func,
            units: Vec::new(),
            extras: vec![BuildExtra::default(); NUM_EXTRAS].into_boxed_slice(),
            labels: Vec::new(),
            table: Box::new([]),
            extras_head: 0,
            keyset,
            dawg: DawgBuilder::new(),
        }
    }

    fn build(mut self) -> Result<Vec<BuildUnit>, BuildError> {
        if self.keyset.has_values() {
            self.build_dawg()?;
            self.build_from_dawg()?;
        } else {
            self.build_from_keyset()?;
        }

        if let Some(progress) = self.progress_func {
            progress(self.keyset.num_keys() + 1, self.keyset.num_keys() + 1);
        }

        Ok(self.units)
    }

    fn num_blocks(&self) -> usize {
        self.units.len() / BLOCK_SIZE
    }

    fn extras(&self, id: Id) -> &BuildExtra {
        // Note: C++ uses % NUM_EXTRAS for indexing the rotating extra array.
        &self.extras[(id as usize) % NUM_EXTRAS]
    }

    fn extras_mut(&mut self, id: Id) -> &mut BuildExtra {
        &mut self.extras[(id as usize) % NUM_EXTRAS]
    }

    // --- DAWG Build Path ---
    fn build_dawg(&mut self) -> Result<(), BuildError> {
        self.dawg.init();
        for i in 0..self.keyset.num_keys() {
            self.dawg.insert(self.keyset.key(i), self.keyset.value(i))?;
            if let Some(progress) = self.progress_func {
                if i==125029 {
                    print!("");
                }
                progress(i + 1, self.keyset.num_keys() + 1);
            }
        }
        self.dawg.finish();
        Ok(())
    }

    fn build_from_dawg(&mut self) -> Result<(), BuildError> {
        let mut num_units = 1;
        while num_units < self.dawg.units.len() {
            num_units <<= 1;
        }
        self.units.reserve(num_units);

        let num_intersections = self.dawg.num_intersections();
        // The table stores the offset for each unique intersection (DAWG state).
        self.table = vec![0; num_intersections].into_boxed_slice();

        // Root node setup (index 0)
        self.reserve_id(0);
        self.extras_mut(0).is_used = true;
        self.units[0].set_offset(1)?; // base=1, offset=1^0=1
        self.units[0].set_label(0); // '\0' (dummy label)

        if self.dawg.child(self.dawg.root()) != 0 {
            self.build_from_dawg_recursive(self.dawg.root(), 0)?;
        }

        self.fix_all_blocks();
        Ok(())
    }

    fn build_from_dawg_recursive(&mut self, dawg_id: Id, dic_id: Id) -> Result<(), BuildError> {
        let dawg_child_id = self.dawg.child(dawg_id);
        
        // Intersection check: If the child of this DAWG node is an intersection
        if self.dawg.is_intersection(dawg_child_id) {
            let intersection_id = self.dawg.intersection_id(dawg_child_id) as usize;
            let offset = self.table[intersection_id];
            
            if offset != 0 {
                let rel_offset = offset ^ dic_id;
                
                // The C++ check: If the relative offset fits the small encoding.
                if (rel_offset & UPPER_MASK == 0) || (rel_offset & LOWER_MASK == 0) {
                    if self.dawg.is_leaf(dawg_child_id) {
                        self.units[dic_id as usize].set_has_leaf(true);
                    }
                    self.units[dic_id as usize].set_offset(rel_offset)?;
                    return Ok(());
                }
            }
        }

        let offset = self.arrange_from_dawg(dawg_id, dic_id)?;
        
        // If this DAWG child is a new intersection, save its offset to the table.
        if self.dawg.is_intersection(dawg_child_id) {
            self.table[self.dawg.intersection_id(dawg_child_id) as usize] = offset;
        }

        // Recurse on children
        let mut current_dawg_child = self.dawg.child(dawg_id);
        while current_dawg_child != 0 {
            let child_label = self.dawg.label(current_dawg_child);
            if child_label != 0 { // Skip null terminator leaf, it's handled by has_leaf flag
                self.build_from_dawg_recursive(current_dawg_child, offset ^ (child_label as Id))?;
            }
            current_dawg_child = self.dawg.sibling(current_dawg_child);
        }
        Ok(())
    }

    fn arrange_from_dawg(&mut self, dawg_id: Id, dic_id: Id) -> Result<Id, BuildError> {
        self.labels.clear();
        let mut dawg_child_id = self.dawg.child(dawg_id);
        // Collect both labels and DAWG IDs before performing mutable operations on self
        let mut dawg_children = Vec::new(); 
        while dawg_child_id != 0 {
            self.labels.push(self.dawg.label(dawg_child_id));
            dawg_children.push(dawg_child_id); // Store the DAWG ID
            dawg_child_id = self.dawg.sibling(dawg_child_id);
        }

        // FIX: Clone the labels to release the immutable borrow on self
        let children_labels = self.labels.clone(); 
        
        let offset = self.find_valid_offset(dic_id);
        self.units[dic_id as usize].set_offset(dic_id ^ offset)?;

        // Use the cloned labels and stored DAWG IDs for correct arrangement
        for (i, &label) in children_labels.iter().enumerate() { 
            let dic_child_id = offset ^ (label as Id);
            let current_dawg_child = dawg_children[i];

            self.reserve_id(dic_child_id); // Mutable borrow now OK

            if self.dawg.is_leaf(current_dawg_child) {
                self.units[dic_id as usize].set_has_leaf(true);
                self.units[dic_child_id as usize].set_value(self.dawg.value(current_dawg_child));
            } else {
                self.units[dic_child_id as usize].set_label(label);
            }
        }
        self.extras_mut(offset).is_used = true;
        Ok(offset)
    }

    // --- Keyset Build Path (no values) ---
    fn build_from_keyset(&mut self) -> Result<(), BuildError> {
        let mut num_units = 1;
        while num_units < self.keyset.num_keys() {
            num_units <<= 1;
        }
        self.units.reserve(num_units);

        // Root node setup
        self.reserve_id(0);
        self.extras_mut(0).is_used = true;
        self.units[0].set_offset(1)?;
        self.units[0].set_label(0); // '\0'

        if self.keyset.num_keys() > 0 {
            self.build_from_keyset_recursive(0, self.keyset.num_keys(), 0, 0)?;
        }

        self.fix_all_blocks();
        Ok(())
    }

    fn build_from_keyset_recursive(
        &mut self,
        begin: usize,
        end: usize,
        depth: usize,
        dic_id: Id,
    ) -> Result<(), BuildError> {
        let offset = self.arrange_from_keyset(begin, end, depth, dic_id)?;

        // Skip keys that ended (null terminator at this depth)
        let mut child_begin = begin;
        while child_begin < end && self.keyset.key_byte(child_begin, depth) == 0 {
            child_begin += 1;
        }
        if child_begin == end {
            return Ok(());
        }

        let mut last_begin = child_begin;
        let mut last_label = self.keyset.key_byte(child_begin, depth);

        for i in (child_begin + 1)..end {
            let label = self.keyset.key_byte(i, depth);
            if label != last_label {
                self.build_from_keyset_recursive(
                    last_begin,
                    i,
                    depth + 1,
                    offset ^ (last_label as Id),
                )?;
                last_begin = i;
                last_label = label;
            }
        }
        // Recurse for the last segment
        self.build_from_keyset_recursive(
            last_begin,
            end,
            depth + 1,
            offset ^ (last_label as Id),
        )?;
        Ok(())
    }

    fn arrange_from_keyset(
        &mut self,
        begin: usize,
        end: usize,
        depth: usize,
        dic_id: Id,
    ) -> Result<Id, BuildError> {
        self.labels.clear();
        let mut value = -1;

        for i in begin..end {
            let label = self.keyset.key_byte(i, depth);
            if label == 0 {
                // Check for invalid null byte
                if depth < self.keyset.key_len(i) {
                        return Err(BuildError::from("Failed to build double-array: invalid null character"));
                }
                // Check for negative value
                let val = self.keyset.value(i);
                if val < 0 {
                    return Err(BuildError::from("Failed to build double-array: negative value"));
                }
                // All end-keys here must have the same value (for non-value mode, this is just the index)
                if value == -1 {
                    value = val;
                }
                // Value consistency check
                if value != val {
                    return Err(BuildError::from("Failed to build double-array: inconsistent value for the same key"));
                }
                if let Some(progress) = self.progress_func {
                    progress(i + 1, self.keyset.num_keys() + 1);
                }
            }

            // Collect unique labels for the children
            if self.labels.is_empty() {
                self.labels.push(label);
            } else if label != *self.labels.last().unwrap() {
                // Keys MUST be sorted lexicographically
                if label < *self.labels.last().unwrap() {
                    return Err(BuildError::from("Failed to build double-array: wrong key order"));
                }
                self.labels.push(label);
            }
        }

        // FIX: Clone the labels to release the immutable borrow on self
        let children_labels = self.labels.clone();

        // Find an offset that fits the children labels into unused slots
        let offset = self.find_valid_offset(dic_id);
        self.units[dic_id as usize].set_offset(dic_id ^ offset)?;

        // Place the children/leaf units
        for &label in &children_labels { // <-- Iterate over the cloned vector
            let dic_child_id = offset ^ (label as Id);
            self.reserve_id(dic_child_id); // <--- Mutable borrow now OK

            if label == 0 {
                self.units[dic_id as usize].set_has_leaf(true);
                self.units[dic_child_id as usize].set_value(value);
            } else {
                self.units[dic_child_id as usize].set_label(label);
            }
        }
        self.extras_mut(offset).is_used = true;
        Ok(offset)
    }

    // --- Common Builder Logic ---

    fn find_valid_offset(&self, id: Id) -> Id {
        // Check if the extra pool is empty (or appears to be)
        if self.extras_head >= self.units.len() as Id {
            // Return the 'first available ID after current units' XORed with lower 8 bits of id
            // This forces a unit expansion.
            return (self.units.len() as Id) | (id & LOWER_MASK);
        }

        // Iterate over the free list
        let mut unfixed_id = self.extras_head;
        loop {
            // self.labels[0] is guaranteed to be the first (smallest) label
            let offset = unfixed_id ^ (self.labels[0] as Id);
            if self.is_valid_offset(id, offset) {
                return offset;
            }
            unfixed_id = self.extras(unfixed_id).next;
            if unfixed_id == self.extras_head {
                break;
            }
        }
        
        // If no offset found in the free list, force an expansion.
        (self.units.len() as Id) | (id & LOWER_MASK)
    }

    fn is_valid_offset(&self, id: Id, offset: Id) -> bool {
        // Check if the offset slot itself is used by another BASE
        if self.extras(offset).is_used {
            return false;
        }

        // Check if the relative offset is encodable (fits the small encoding rules)
        let rel_offset = id ^ offset;
        if (rel_offset & LOWER_MASK != 0) && (rel_offset & UPPER_MASK != 0) {
            return false;
        }

        // Check if all potential children slots are available (unfixed)
        // Skip the first label (self.labels[0]) because it's checked by the `extras(offset).is_used` above.
        for &label in self.labels.iter().skip(1) {
            if self.extras(offset ^ (label as Id)).is_fixed {
                return false;
            }
        }
        true
    }

    fn reserve_id(&mut self, id: Id) {
        // Expand unit array if necessary
        if id as usize >= self.units.len() {
            self.expand_units();
        }

        // Remove from free list if it was the head
        if id == self.extras_head {
            self.extras_head = self.extras(id).next;
            // If it was the only element in the list, set head to a dummy high value
            if self.extras_head == id {
                self.extras_head = self.units.len() as Id;
            }
        }

        // Remove from free list
        let prev = self.extras(id).prev;
        let next = self.extras(id).next;
        self.extras_mut(prev).next = next;
        self.extras_mut(next).prev = prev;
        self.extras_mut(id).is_fixed = true;
    }

    fn expand_units(&mut self) {
        let src_num_units = self.units.len();
        let src_num_blocks = self.num_blocks();

        let dest_num_units = src_num_units + BLOCK_SIZE;
        let dest_num_blocks = src_num_blocks + 1;

        // Fix old blocks before they rotate out of the extra array
        if dest_num_blocks > NUM_EXTRA_BLOCKS {
            self.fix_block(src_num_blocks - NUM_EXTRA_BLOCKS);
        }

        // Resize the units vector
        self.units
            .resize(dest_num_units, BuildUnit::default());

        // Reset the 'extra' info for the new units (only if needed due to rotation)
        if dest_num_blocks > NUM_EXTRA_BLOCKS {
            for id in src_num_units..dest_num_units {
                *self.extras_mut(id as Id) = BuildExtra::default();
            }
        }

        // Link the new units into a new segment of the free list
        for i in (src_num_units + 1)..dest_num_units {
            self.extras_mut(i as Id - 1).next = i as Id;
            self.extras_mut(i as Id).prev = i as Id - 1;
        }

        // Circular link for the new block
        let start_id = src_num_units as Id;
        let end_id = (dest_num_units - 1) as Id;
        
        self.extras_mut(start_id).prev = end_id;
        self.extras_mut(end_id).next = start_id;


        // Integrate the new block into the main free list
        if self.extras_head < src_num_units as Id {
            let extras_head_prev = self.extras(self.extras_head).prev;
            
            // Link new block start to old block end
            self.extras_mut(start_id).prev = extras_head_prev;
            
            // Link new block end to old head
            self.extras_mut(end_id).next = self.extras_head;

            // Update old pointers to point to the new block
            self.extras_mut(extras_head_prev).next = start_id;
            self.extras_mut(self.extras_head).prev = end_id;
        } else {
                // The old head was already past the current array size, so the new block becomes the head
                self.extras_head = start_id;
        }
    }

    fn fix_all_blocks(&mut self) {
        let begin = if self.num_blocks() > NUM_EXTRA_BLOCKS {
            self.num_blocks() - NUM_EXTRA_BLOCKS
        } else {
            0
        };
        let end = self.num_blocks();

        for block_id in begin..end {
            self.fix_block(block_id);
        }
    }

    fn fix_block(&mut self, block_id: usize) {
        let begin = (block_id * BLOCK_SIZE) as Id;
        let end = begin + (BLOCK_SIZE as Id);

        // 1. Find the first available (unused) offset in this block
        let mut unused_offset = 0;
        for offset in begin..end {
            if !self.extras(offset).is_used {
                unused_offset = offset;
                break;
            }
        }

        // 2. Fix all unfixed units in this block
        for id in begin..end {
            if !self.extras(id).is_fixed {
                self.reserve_id(id);
                // The label is set to `id ^ unused_offset` to form a BASE/CHECK pair
                // that points nowhere useful, but fills the slot permanently.
                self.units[id as usize].set_label((id ^ unused_offset) as u8);
            }
        }
    }
}

/// Builder for a Darts double-array.
pub struct Builder {
    progress_func: Option<fn(usize, usize)>,
}

impl Builder {
    /// Creates a new builder.
    pub fn new() -> Self {
        Self {
            progress_func: None,
        }
    }

    /// Sets a callback function to report progress.
    /// The callback takes (current_item, total_items).
    pub fn with_progress(mut self, func: fn(usize, usize)) -> Self {
        self.progress_func = Some(func);
        self
    }

    /// Builds the double-array from a set of keys.
    /// Keys **MUST** be sorted in ascending lexicographical order.
    ///
    /// - `keys`: A slice of byte slices (`&[&[u8]]`).
    /// - `values`: An optional slice of `i32` values.
    ///   - If `Some`, values must align with keys. All values must be >= 0.
    ///   - If `None`, values 0, 1, 2... will be assigned based on key index.
    pub fn build(
        &self,
        keys: &[&[u8]],
        values: Option<&[Value]>,
    ) -> Result<Vec<u8>, BuildError> {
        let keyset = Keyset { keys, values };
        let builder = DoubleArrayBuilder::new(keyset, self.progress_func);

        let build_units = builder.build()?;

        // The build process is done. `build_units` is a `Vec<BuildUnit>`.
        // We need to convert it to `Vec<u8>`.
        // `BuildUnit` and `DoubleArrayUnit` are both `#[repr(transparent)]`
        // wrappers around `u32`. We can safely transmute them.

        // The following uses a safe, albeit verbose, approach to transmute Vec<T> to Vec<U>
        // where T and U have the same size and alignment, and T is guaranteed to be a valid U.
        // Since BuildUnit is #[repr(transparent)] u32, this is safe.

        let mut units_u32: Vec<u32> = unsafe {
            // Get the raw parts and capacity for the BuildUnit vector
            let ptr = build_units.as_ptr() as *mut u32;
            let len = build_units.len();
            let cap = build_units.capacity();

            // Prevent the original Vec<BuildUnit> from running its destructor
            std::mem::forget(build_units);

            // Reconstruct the memory as Vec<u32>
            Vec::from_raw_parts(ptr, len, cap)
        };

        // Convert Vec<u32> to Vec<u8> (standard in Rust for `u32` slices/vectors)
        let bytes: Vec<u8>;
        let ptr = units_u32.as_mut_ptr();
        let len = units_u32.len();
        let cap = units_u32.capacity();

        unsafe {
            // Forget the `Vec<u32>`
            std::mem::forget(units_u32);

            // Reconstruct the memory as `Vec<u8>`.
            bytes = Vec::from_raw_parts(
                ptr as *mut u8,
                len * 4,
                cap * 4,
            );
        }

        Ok(bytes)
    }
}








// Example of how to use the builder
#[cfg(test)]
mod tests {
    use super::Builder;
    use super::{DoubleArray, Match};

    #[test]
    fn test_build_and_search_no_values() {
        let keys = [
            "a".as_bytes(),
            "abc".as_bytes(),
            "b".as_bytes(),
            "bp".as_bytes(),
            "c".as_bytes(),
        ];

        let builder = Builder::new();
        let darts_data = builder.build(&keys, None).unwrap();

        let da = DoubleArray::new(&darts_data).unwrap();

        let mut node_pos = 0;
        let mut key_pos = 0;
        assert_eq!(da.traverse("a", &mut node_pos, &mut key_pos), 0);
        
        node_pos = 0;
        key_pos = 0;
        assert_eq!(da.traverse("abc", &mut node_pos, &mut key_pos), 1);
        
        node_pos = 0;
        key_pos = 0;
        assert_eq!(da.traverse("b", &mut node_pos, &mut key_pos), 2);

        node_pos = 0;
        key_pos = 0;
        assert_eq!(da.traverse("bp", &mut node_pos, &mut key_pos), 3);

        node_pos = 0;
        key_pos = 0;
        assert_eq!(da.traverse("c", &mut node_pos, &mut key_pos), 4);
        
        node_pos = 0;
        key_pos = 0;
        // "ab" is a prefix but not a key
        assert_eq!(da.traverse("ab", &mut node_pos, &mut key_pos), -1);
        
        node_pos = 0;
        key_pos = 0;
        // "d" does not exist
        assert_eq!(da.traverse("d", &mut node_pos, &mut key_pos), -2);
    }

    #[test]
    fn test_build_and_search_with_values() {
        let keys = [
            "ALGOL".as_bytes(),
            "ANSI".as_bytes(),
            "ARCO".as_bytes(),
            "ARPA".as_bytes(),
            "ARPANET".as_bytes(),
            "ASCII".as_bytes(),
        ];
        let values = [10, 20, 30, 40, 50, 60];

        let builder = Builder::new();
        let darts_data = builder.build(&keys, Some(&values)).unwrap();

        let da = DoubleArray::new(&darts_data).unwrap();

        let mut node_pos = 0;
        let mut key_pos = 0;
        assert_eq!(da.traverse("ALGOL", &mut node_pos, &mut key_pos), 10);
        
        node_pos = 0;
        key_pos = 0;
        assert_eq!(da.traverse("ARPANET", &mut node_pos, &mut key_pos), 50);

        node_pos = 0;
        key_pos = 0;
        assert_eq!(da.traverse("ASCII", &mut node_pos, &mut key_pos), 60);

        node_pos = 0;
        key_pos = 0;
        // "AR" is not a key
        assert_eq!(da.traverse("AR", &mut node_pos, &mut key_pos), -1);

        node_pos = 0;
        key_pos = 0;
        // "FOO" does not exist
        assert_eq!(da.traverse("FOO", &mut node_pos, &mut key_pos), -2);
    }

     #[test]
    fn test_common_prefix_search() {
        let keys = [
            "a".as_bytes(),
            "ab".as_bytes(),
            "abc".as_bytes(),
        ];
        let values = [1, 2, 3];

        let builder = Builder::new();
        let darts_data = builder.build(&keys, Some(&values)).unwrap();
        let da = DoubleArray::new(&darts_data).unwrap();
        
        let mut results = [Match::default(); 5];
        let num_results = da.common_prefix_search("abcd", 0, &mut results);

        assert_eq!(num_results, 3);
        assert_eq!(results[0].value, 1);
        assert_eq!(results[0].length, 1); // "a"
        assert_eq!(results[1].value, 2);
        assert_eq!(results[1].length, 2); // "ab"
        assert_eq!(results[2].value, 3);
        assert_eq!(results[2].length, 3); // "abc"
    }
    
    #[test]
    fn test_large_build() {
        let num_keys = 400_000; // test totally 400k keys
        let mut keys_sorted: Vec<String> = Vec::with_capacity(num_keys);

        let prefix = "key";
        let suffix = "喵呜";

        for i in 0..num_keys {
             // Pad with zeros to ensure lexicographical sort matches index order
             keys_sorted.push(format!("{prefix}{i:0>6}{suffix}"));
        }

        let key_slices: Vec<&[u8]> = keys_sorted.iter().map(|s| s.as_bytes()).collect();
        let values: Vec<i32> = (0..num_keys as i32).collect();

        // This test will run the full build logic for keys.
        let builder = Builder::new();
        let darts_data = builder.build(&key_slices, Some(&values)).unwrap();
        
        let da = DoubleArray::new(&darts_data).unwrap();

        // Test first key
        let mut node_pos = 0;
        let mut key_pos = 0;
        assert_eq!(da.traverse(format!("{prefix}000000{suffix}"), &mut node_pos, &mut key_pos), 0);
        
        // Test a key in the middle
        node_pos = 0;
        key_pos = 0;
        assert_eq!(da.traverse(format!("{prefix}050000{suffix}"), &mut node_pos, &mut key_pos), 50000);
        
        // Test large num key
        node_pos = 0;
        key_pos = 0;
        assert_eq!(da.traverse(format!("{prefix}399999{suffix}"), &mut node_pos, &mut key_pos), 399999);
        
        // Test non-existent key
        node_pos = 0;
        key_pos = 0;
        let out_bound_key_num = num_keys + 1;
        assert_eq!(da.traverse(format!("{prefix}{out_bound_key_num}{suffix}"), &mut node_pos, &mut key_pos), -2);
    }
}