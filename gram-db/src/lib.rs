//! Rust implementation of rime::GramDb loader and querier.

use darts::{DoubleArray, Match};
use memmap2::Mmap;
use std::fs::{File, OpenOptions};
use std::io::{Write, Seek, SeekFrom};
use std::path::Path;
use std::mem;


/// C++: `rime::grammar::Metadata`
/// This struct MUST match the C++ layout exactly.
#[repr(C)]
#[derive(Clone, Copy, Debug)]
struct GramDbMetadata {
    /// C++: `char format[kFormatMaxLength]`
    format: [u8; 32],
    /// C++: `uint32_t db_checksum`
    db_checksum: u32,
    /// C++: `uint32_t double_array_size` (number of units)
    double_array_size: u32,
    /// C++: `OffsetPtr<char> double_array`
    /// This is the offset *from this field's location* to the array data.
    double_array_offset: u32,
}

// C++: `kMaxResults = 8`
pub const K_MAX_RESULTS: usize = 8;
// C++: `kValueScale = 10000`
pub const K_VALUE_SCALE: f64 = 10000.0;

/// The `GramDb` holds the memory-mapped file and the
/// `DoubleArray` view into it.
pub struct GramDb<'a> {
    // We must hold the Mmap to keep the data alive.
    // The 'a lifetime depends on this Mmap.
    #[allow(dead_code)]
    mmap: Mmap,

    /// The DoubleArray trie, a view into the Mmap data.
    pub trie: DoubleArray<'a>,
}

impl<'a> GramDb<'a> {
    /// Opens and maps a `.gram` file, parsing its metadata.
    pub fn open(path: &Path) -> Result<Self, &'static str> {
        let file = File::open(path).map_err(|_| "Failed to open file")?;
        let mmap = unsafe { Mmap::map(&file).map_err(|_| "Failed to mmap file")? };

        let data: &'static [u8] = unsafe {
            // We leak the Mmap's data slice to get a 'static lifetime.
            // This is a common pattern when self-referential structs
            // are needed. The `GramDb` struct holds the `Mmap` itself,
            // ensuring the data is valid as long as `GramDb` is alive.
            // The 'a lifetime is tied to the `GramDb` struct itself.
            let slice = &mmap[..];
            std::mem::transmute::<&[u8], &'static [u8]>(slice)
        };

        // 1. Get metadata (at offset 0)
        let metadata_size = mem::size_of::<GramDbMetadata>();
        let metadata_bytes = &data
            .get(..metadata_size)
            .ok_or("File too small for metadata")?;

        let metadata: &GramDbMetadata = unsafe {
            let ptr = metadata_bytes.as_ptr();
            // Align check (good practice, but mmap at offset 0 is aligned)
            if ptr as usize % mem::align_of::<GramDbMetadata>() != 0 {
                return Err("Metadata is not aligned");
            }
            // Cast the pointer to the correct struct reference
            &*(ptr as *const GramDbMetadata)
        };

        // 2. Check format
        let format_str = String::from_utf8_lossy(&metadata.format);
        if !format_str.starts_with("Rime::Grammar/") {
            return Err("Invalid .gram file format");
        }

        // 3. Get array data
        // C++: `metadata_->double_array.get()`
        // This is `(char*)(&metadata->double_array) + metadata->double_array_offset`
        // `&metadata->double_array` is at offset 40 in the file.
        const METADATA_OFFSET_OF_ARRAY_OFFSET: usize = 32 + 4 + 4; // 40

        let array_data_start_pos =
            METADATA_OFFSET_OF_ARRAY_OFFSET + (metadata.double_array_offset as usize);

        let num_units = metadata.double_array_size as usize;
        let num_bytes = num_units * mem::size_of::<u32>();

        let array_data_end_pos = array_data_start_pos
            .checked_add(num_bytes)
            .ok_or("Array data start + size overflows")?;

        let array_slice = data
            .get(array_data_start_pos..array_data_end_pos)
            .ok_or("Array data bounds exceed file size")?;

        // 4. Create Darts trie
        let trie = DoubleArray::new(array_slice)?;

        // The 'static lifetime is "unsafe" but we manage it by
        // holding the Mmap.
        Ok(GramDb { mmap, trie })
    }

    /// C++: `int GramDb::Lookup(...)`
    pub fn lookup(
        &self,
        context: &str,
        word: &str,
        results: &mut [Match; K_MAX_RESULTS],
    ) -> usize {
        let mut node_pos = 0; // root node
        let mut key_pos = 0;

        let context = GramDbKey::encode(context);
        let word = GramDbKey::encode(word);

        // 1. Follow the context string
        // C++: trie_->traverse(context.c_str(), node_pos, key_pos);
        self.trie.traverse(&context, &mut node_pos, &mut key_pos);

        // 2. Check if context was fully matched
        // C++: if (key_pos == context.length())
        if key_pos == context.len() {
            // 3. Perform prefix search for the word from that node
            // C++: return trie_->commonPrefixSearch(..., node_pos);
            self.trie.common_prefix_search(&word, node_pos, results)
        } else {
            // Context not found, no matches possible
            0
        }
    }
}

pub struct GramDbValue;
impl GramDbValue {
    pub fn scale(value: f64) -> i32 {
        (value.ln() * K_VALUE_SCALE).max(0.0) as i32
    }

    pub fn from_scaled(value: i32) -> f64 {
        (value as f64/K_VALUE_SCALE).exp()
    }
}

pub struct GramDbKey;
impl GramDbKey {
    /// C++: rime::grammar::encode
    /// Converts a standard UTF-8 string into the GramDb's compact key encoding.
    pub fn encode(s: &str) -> Vec<u8> {
        let mut result = Vec::new();
        let mut chars = s.chars();
        
        while let Some(c) = chars.next() {
            let u = c as u32;
            
            if u < 0x80 {
                if u == 0 {
                    result.push(0xE0);
                } else {
                    result.push(u as u8);
                }
            } else if u >= 0x4000 && u < 0xA000 {
                if (u & 0xFF) == 0 {
                    result.push(0xE1);
                    result.push(((u >> 8) + 0x40) as u8);
                } else {
                    result.push(((u >> 8) + 0x40) as u8);
                    result.push((u & 0xFF) as u8);
                }
            } else {
                let mut bits = 32;
                let mut temp_u = u;
                
                // 找到最高有效位
                while bits > 0 && (temp_u & 0xFE000000) == 0 {
                    bits -= 7;
                    temp_u <<= 7;
                }
                
                let bytes_to_encode = (bits + 6) / 7;
                result.push(0xE0 | bytes_to_encode as u8);
                
                let temp_u = u;
                for i in 0..bytes_to_encode {
                    let shift_amount = 25 - (7 * i as u32);
                    result.push((((temp_u >> shift_amount) & 0x7F) | 0x80) as u8);
                }
            }
        }
        
        result
    }

    pub fn decode(bytes: &[u8]) -> String {
        let mut result = String::new();
        let mut i = 0;
        
        while i < bytes.len() {
            let byte = bytes[i];
            
            if byte < 0x80 {
                // ASCII 字符
                if byte == 0 {
                    // 特殊情况：null 字符
                    result.push('\0');
                } else {
                    result.push(byte as char);
                }
                i += 1;
            } else if byte == 0xE0 {
                // 特殊情况：null 字符
                result.push('\0');
                i += 1;
            } else if byte == 0xE1 {
                // 两字节编码，低 8 位为 0
                if i + 1 >= bytes.len() {
                    break; // 数据不完整
                }
                let high_byte = bytes[i + 1];
                let u = ((high_byte as u32).wrapping_sub(0x40)) << 8;
                if let Some(c) = char::from_u32(u) {
                    result.push(c);
                }
                i += 2;
            } else if (byte & 0xF0) == 0xE0 {
                // 变长编码
                let bytes_to_decode = (byte & 0x0F) as usize;
                if bytes_to_decode == 0 || i + bytes_to_decode > bytes.len() {
                    break; // 数据不完整或无效
                }
                
                let mut u: u32 = 0;
                for j in 1..=bytes_to_decode {
                    let b = bytes[i + j];
                    if (b & 0x80) == 0 {
                        break; // 无效编码
                    }
                    u = (u << 7) | ((b & 0x7F) as u32);
                }
                
                if let Some(c) = char::from_u32(u) {
                    result.push(c);
                }
                i += bytes_to_decode + 1;
            } else {
                // 两字节编码，低 8 位不为 0
                if i + 1 >= bytes.len() {
                    break; // 数据不完整
                }
                let high_byte = byte;
                let low_byte = bytes[i + 1];
                let u = ((high_byte as u32).wrapping_sub(0x40)) << 8 | (low_byte as u32);
                if let Some(c) = char::from_u32(u) {
                    result.push(c);
                }
                i += 2;
            }
        }
        
        result
    }
}


/// Builder for creating GramDb files
pub struct GramDbBuilder {
    data: Vec<(String, f64)>,
    progress_func: Option<fn(usize, usize)>,
}

impl GramDbBuilder {
    pub fn new() -> Self {
        Self {
            data: Vec::new(),
            progress_func: None,
        }
    }

    /// Set progress callback function
    /// The callback takes (current_item, total_items)
    pub fn with_progress(mut self, func: fn(usize, usize)) -> Self {
        self.progress_func = Some(func);
        self
    }

    /// Add training data (context-word pairs with frequency/scores)
    pub fn extend_data(&mut self, data: Vec<(String, f64)>) {
        self.data.extend(data);
    }

    /// Build the GramDb file
    pub fn build(&self, path: &Path) -> Result<(), &'static str> {
        if self.data.is_empty() {
            return Err("No data to build");
        }

        let mut raw_data: Vec<(Vec<u8>, i32)> = self.data.iter().map(
            |(key,  value)| {
                (
                    GramDbKey::encode(key),
                    GramDbValue::scale(*value)
                )
            }
        ).collect();
        // 根据 tuple.0 (Vec<u8>) 排序，不进行克隆
        raw_data.sort_by(|(key1, _), (key2, _)| key1.cmp(key2));

        let (raw_keys, raw_values): (Vec<Vec<u8>>, Vec<i32>) = raw_data.into_iter().unzip();
        let raw_keys: Vec<&[u8]> = raw_keys.iter().map(|v| v.as_slice()).collect();


        // Build the double-array trie
        let mut builder = darts::Builder::new();
        if let Some(f) = self.progress_func {
            builder = builder.with_progress(f);
        }
        let darts_data = builder.build(&raw_keys, Some(&raw_values))
            .map_err(|_| "Failed to build double-array trie")?;

        // Create the file
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(path)
            .map_err(|_| "Failed to create gram db file")?;

        // Calculate sizes
        let array_size = darts_data.len() / 4; // number of u32 units
        let image_size = darts_data.len();
        const K_RESERVED_SIZE: usize = 1024; // C++: kReservedSize = 1024

        // Write initial file content with enough capacity
        let total_size = image_size + K_RESERVED_SIZE;
        file.set_len(total_size as u64)
            .map_err(|_| "Failed to set file length")?;

        // Seek to beginning to write metadata
        file.seek(SeekFrom::Start(0))
            .map_err(|_| "Failed to seek to file start")?;

        // Create and write metadata
        let mut metadata = GramDbMetadata {
            format: [0; 32],
            db_checksum: 0, // Not implemented in C++ version
            double_array_size: array_size as u32,
            double_array_offset: 0, // Will be calculated relative to its own position
        };

        // Format string
        let format_str = "Rime::Grammar/1.0";
        let format_bytes = format_str.as_bytes();
        let copy_len = format_bytes.len().min(metadata.format.len() - 1);
        metadata.format[..copy_len].copy_from_slice(&format_bytes[..copy_len]);

        // Calculate double_array offset
        // The offset is from the double_array_offset field itself to the array data
        // The double_array_offset field is at offset 40 in the file
        // We'll place the array data right after the metadata
        let metadata_size = mem::size_of::<GramDbMetadata>();
        metadata.double_array_offset = (metadata_size - 40) as u32; // 40 is offset of double_array_offset field

        // Write metadata
        let metadata_bytes = unsafe {
            std::slice::from_raw_parts(
                &metadata as *const _ as *const u8,
                mem::size_of::<GramDbMetadata>(),
            )
        };
        file.write_all(metadata_bytes)
            .map_err(|_| "Failed to write metadata")?;

        // Write double-array data at the calculated position
        let array_start_pos = metadata_size;
        file.seek(SeekFrom::Start(array_start_pos as u64))
            .map_err(|_| "Failed to seek to array data position")?;
        file.write_all(&darts_data)
            .map_err(|_| "Failed to write double-array data")?;

        // // Report final progress
        // if let Some(progress) = self.progress_func {
        //     progress(keys.len() + 1, keys.len() + 1);
        // }

        // Flush to ensure data is written
        file.flush().map_err(|_| "Failed to flush file")?;

        Ok(())
    }
}


#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn test_build_and_query() {
        // Test data similar to what would be used in C++
        let test_data = vec![
            ("a", 1.0),
            ("ab", 2.0),
            ("abc", 3.0),
            ("b", 1.5),
            ("bc", 2.5),
        ].into_iter()
         .map(|(s, f)| (s.to_string(), f))
         .collect();

        let mut builder = GramDbBuilder::new();
        builder.extend_data(test_data);

        let test_path = Path::new("test.gram");
        
        // Build the gram db
        assert!(builder.build(test_path).is_ok());

        // Try to open and query
        let gram_db = GramDb::open(test_path).expect("Failed to open built gram db");

        // Test lookup functionality
        let mut results = [Match::default(); K_MAX_RESULTS];

        // Test case 1: Context "a", word "b" should find transitions from "a" context
        let num_results = gram_db.lookup("a", "b", &mut results);
        assert!(num_results > 0, "Should find some results for context 'a', word 'b'");

        // Test case 2: Context "ab", word "c" 
        let num_results = gram_db.lookup("ab", "c", &mut results);
        assert!(num_results > 0, "Should find some results for context 'ab', word 'c'");

        // Test case 3: Non-existent context
        let num_results = gram_db.lookup("x", "y", &mut results);
        assert_eq!(num_results, 0, "Should find no results for non-existent context");

        // Clean up
        let _ = fs::remove_file(test_path);
    }

    #[test]
    fn test_build_with_progress() {
        let test_data = vec![
            ("test1", 1.0),
            ("test2", 2.0),
        ].into_iter()
         .map(|(s, f)| (s.to_string(), f))
         .collect();

        
        let progress_func = |current: usize, total: usize| {
            println!("Progress: {}/{}", current, total);
        };

        let mut builder = GramDbBuilder::new().with_progress(progress_func);
        builder.extend_data(test_data);

        let test_path = Path::new("test_progress.gram");
        
        assert!(builder.build(test_path).is_ok());

        // Clean up
        let _ = fs::remove_file(test_path);
    }

    #[test]
    fn test_build_empty_data() {
        let builder = GramDbBuilder::new();
        let test_path = Path::new("test_empty.gram");
        
        assert!(builder.build(test_path).is_err());
    }

    #[test]
    fn test_build_large_data() {
        // Test with larger dataset
        let mut test_data = Vec::with_capacity(21000);
        for i in 00_0000..20_0000 {
            test_data.push((format!("word{:0>7}", i), (i + 1) as f64));
        }

        let mut builder = GramDbBuilder::new().with_progress(|current: usize, total: usize| {
            println!("Progress: {}/{}", current, total);
        });
        // let mut builder = GramDbBuilder::new().with_progress(|current, total| {
        //         let percount = total/100;
        //         if percount==0 {return;}
        //         if current % percount == 0 {
        //             let progress = current / (total/100);
        //             println!("{progress}%, {current}/{total}");
        //         }
        // });
        builder.extend_data(test_data);

        let test_path = Path::new("test_large.gram");
        
        assert!(builder.build(test_path).is_ok());
        
        // Verify we can open it
        assert!(GramDb::open(test_path).is_ok());

        // Clean up
        let _ = fs::remove_file(test_path);
    }

    #[test]
    fn test_metadata_layout() {
        // Verify that our Rust metadata struct matches C++ layout
        assert_eq!(mem::size_of::<GramDbMetadata>(), 44); // 32 + 4 + 4 + 4
        assert_eq!(mem::align_of::<GramDbMetadata>(), 4);
        
        // Verify field offsets match C++
        let metadata = GramDbMetadata {
            format: [0; 32],
            db_checksum: 0,
            double_array_size: 0,
            double_array_offset: 0,
        };
        
        let base = &metadata as *const _ as usize;
        let format_addr = &metadata.format as *const _ as usize;
        let checksum_addr = &metadata.db_checksum as *const _ as usize;
        let array_size_addr = &metadata.double_array_size as *const _ as usize;
        let array_offset_addr = &metadata.double_array_offset as *const _ as usize;
        
        assert_eq!(format_addr - base, 0);
        assert_eq!(checksum_addr - base, 32);
        assert_eq!(array_size_addr - base, 36);
        assert_eq!(array_offset_addr - base, 40);
    }
}