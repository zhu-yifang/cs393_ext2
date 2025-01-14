#![feature(int_roundings)]

mod structs;
use crate::structs::{BlockGroupDescriptor, DirectoryEntry, Inode, Superblock};
use null_terminated::Nul;
use null_terminated::NulStr;
use rustyline::{DefaultEditor, Result};
use std::fmt;
use std::io::{self, Write};
use std::mem;
use uuid::Uuid;
use zerocopy::ByteSlice;

#[repr(C)]
#[derive(Debug)]
pub struct Ext2 {
    pub superblock: &'static Superblock,
    pub block_groups: &'static mut [BlockGroupDescriptor],
    pub blocks: Vec<&'static [u8]>,
    pub block_size: usize,
    pub uuid: Uuid,
    pub block_offset: usize, // <- our "device data" actually starts at this index'th block of the device
                             // so we have to subtract this number before indexing blocks[]
}

const EXT2_MAGIC: u16 = 0xef53;
const EXT2_START_OF_SUPERBLOCK: usize = 1024;
const EXT2_END_OF_SUPERBLOCK: usize = 2048;

impl Ext2 {
    pub fn new<B: ByteSlice + std::fmt::Debug>(device_bytes: B, start_addr: usize) -> Ext2 {
        // https://wiki.osdev.org/Ext2#Superblock
        // parse into Ext2 struct - without copying

        // the superblock goes from bytes 1024 -> 2047
        let header_body_bytes = device_bytes.split_at(EXT2_END_OF_SUPERBLOCK);

        let superblock = unsafe {
            &*(header_body_bytes
                .0
                .split_at(EXT2_START_OF_SUPERBLOCK)
                .1
                .as_ptr() as *const Superblock)
        };
        assert_eq!(superblock.magic, EXT2_MAGIC);
        // at this point, we strongly suspect these bytes are indeed an ext2 filesystem

        println!("superblock:\n{:?}", superblock);
        println!("size of Inode struct: {}", mem::size_of::<Inode>());

        let block_group_count = superblock
            .blocks_count
            .div_ceil(superblock.blocks_per_group) as usize;

        // not sure about the unit of block_size, bits or bytes?
        let block_size: usize = 1024 << superblock.log_block_size;
        println!(
            "there are {} block groups and block_size = {}",
            block_group_count, block_size
        );
        let block_groups_rest_bytes = header_body_bytes.1.split_at(block_size);

        let block_groups = unsafe {
            std::slice::from_raw_parts_mut(
                block_groups_rest_bytes.0.as_ptr() as *mut BlockGroupDescriptor,
                block_group_count,
            )
        };

        println!("block group 0: {:?}", block_groups[0]);

        let blocks = unsafe {
            std::slice::from_raw_parts(
                block_groups_rest_bytes.1.as_ptr() as *const u8,
                // would rather use: device_bytes.as_ptr(),
                superblock.blocks_count as usize * block_size,
            )
        }
        .chunks(block_size)
        .collect::<Vec<_>>();

        let offset_bytes = (blocks[0].as_ptr() as usize) - start_addr;
        let block_offset = offset_bytes / block_size;
        let uuid = Uuid::from_bytes(superblock.fs_id);
        Ext2 {
            superblock,
            block_groups,
            blocks,
            block_size,
            uuid,
            block_offset,
        }
    }

    // given a (1-indexed) inode number, return that #'s inode structure
    // the inode number is a unique identifier among the entire filesystem
    pub fn get_inode(&self, inode: usize) -> &Inode {
        // find the block group that contains the inode
        let group: usize = (inode - 1) / self.superblock.inodes_per_group as usize;
        // find the index of the inode within the block group
        let index: usize = (inode - 1) % self.superblock.inodes_per_group as usize;

        // println!("in get_inode, inode num = {}, index = {}, group = {}", inode, index, group);
        let inode_table_block =
            (self.block_groups[group].inode_table_block) as usize - self.block_offset;
        // println!("in get_inode, block number of inode table {}", inode_table_block);
        let inode_table = unsafe {
            std::slice::from_raw_parts(
                self.blocks[inode_table_block].as_ptr() as *const Inode,
                self.superblock.inodes_per_group as usize,
            )
        };
        // probably want a Vec of BlockGroups in our Ext structure so we don't have to slice each time,
        // but this works for now.
        // println!("{:?}", inode_table);
        &inode_table[index]
    }

    // A helper function for `read_dir_inode` to read  direct pointers and return the data as a Vec<u8>
    fn read_dir_indir_ptr(&self, block_num: usize) -> std::io::Result<Vec<(usize, &NulStr)>> {
        // indirect pointer points to a block full of direct block numbers/addresses
        // block addresses/numbers stored in the block are all 32-bit
        let indir_block = self.blocks[block_num];
        // this pointer points to the head of the indirect block
        let entry_ptr = indir_block.as_ptr();
        // byte_offset is the offset in bytes from the head of the indirect block, like the index of an array
        let mut byte_offset: isize = 0;
        let mut ret = Vec::new();
        while byte_offset < self.block_size as isize {
            // get direct block number from indirect ptr one at a time
            let directory = unsafe { &*(entry_ptr.offset(byte_offset) as *const DirectoryEntry) };
            // if the inode number is 0, then the entry is empty
            if directory.inode == 0 {
                // println!("inode num: {}", directory.inode_num);
                // println!("name: {}", directory.name);
                return Ok(ret);
            }
            ret.push((directory.inode as usize, &directory.name));
            // move the byte_offset to the next entry
            byte_offset += directory.entry_size as isize;
        }
        Ok(ret)
    }

    // A helper function for `read_dir_inode` read the doubly indirect pointer and return the data as a Vec<u8>
    fn read_dir_doubly_ptr(&self, block_num: usize) -> std::io::Result<Vec<(usize, &NulStr)>> {
        // stores a bunch of singly indirect pointer block numbers
        let doub_block = self.blocks[block_num];
        let entry_ptr = doub_block.as_ptr();
        let mut byte_offset: isize = 0;
        let mut ret = Vec::new();
        while byte_offset < self.block_size as isize {
            let directory = unsafe { &*(entry_ptr.offset(byte_offset) as *const DirectoryEntry) };
            if directory.inode == 0 {
                return Ok(ret);
            }
            let data_from_indir = &(self.read_dir_indir_ptr(directory.inode as usize))
                .expect("error reading indirect pointer");
            ret.extend_from_slice(data_from_indir);
            byte_offset += directory.entry_size as isize;
        }
        Ok(ret)
    }

    // A helper function for `read_file_inode` read the triply indirect pointer and return the data as a Vec<u8>
    fn read_dir_triply_ptr(&self, block_num: usize) -> std::io::Result<Vec<(usize, &NulStr)>> {
        let triply_indir_block = self.blocks[block_num];
        let entry_ptr = triply_indir_block.as_ptr();
        let mut byte_offset: isize = 0;
        let mut ret = Vec::new();
        while byte_offset < self.block_size as isize {
            let directory = unsafe { &*(entry_ptr.offset(byte_offset) as *const DirectoryEntry) };
            if directory.inode == 0 {
                return Ok(ret);
            }
            let data_from_doubly = &(self.read_dir_doubly_ptr(directory.inode as usize))
                .expect("error reading doubly indirect pointer");
            ret.extend_from_slice(data_from_doubly);
            byte_offset += directory.entry_size as isize;
        }
        Ok(ret)
    }

    // given a (1-indexed) inode number, return a list of (inode, name) pairs
    pub fn read_dir_inode(&self, inode: usize) -> std::io::Result<Vec<(usize, &NulStr)>> {
        let mut ret = Vec::new();
        // root is the inode of the directory we're reading
        let root = self.get_inode(inode);
        // println!("in read_dir_inode, #{} : {:?}", inode, root);
        // println!("following direct pointer to data block: {}", root.direct_pointer[0]);
        // entry_ptr is a pointer to the first entry in the directory

        // iterate over all the direct pointers
        for direct_ptr in root.direct_pointer.iter() {
            // <- todo, support large directories
            // if block_num is 0, there are no more blocks -- invalid
            let block_num = *direct_ptr;
            if block_num == 0 {
                return Ok(ret);
            }
            // get the pointer to the first entry in the directory
            let entry_ptr = self.blocks[block_num as usize - self.block_offset].as_ptr();
            // byte_offset is the offset from the start of the directory to the current entry
            let mut byte_offset: isize = 0;
            while byte_offset < self.block_size as isize {
                // <- todo, support large directories
                let directory =
                    unsafe { &*(entry_ptr.offset(byte_offset) as *const DirectoryEntry) };
                // if the directory is empty, we're done
                if directory.inode == 0 {
                    return Ok(ret);
                }
                // println!("{:?}", directory);
                byte_offset += directory.entry_size as isize;
                ret.push((directory.inode as usize, &directory.name));
            }
        }

        // read indirect pointer
        let indirect_ptr = root.indirect_pointer;
        if indirect_ptr == 0 {
            return Ok(ret);
        }
        let indir_block_num = indirect_ptr as usize - self.block_offset;
        let data = self
            .read_dir_indir_ptr(indir_block_num)
            .expect("error reading indirect pointer");
        ret.extend_from_slice(&data);

        // read doubly indirect pointer
        let doub_indir_ptr = root.doubly_indirect;
        if doub_indir_ptr == 0 {
            return Ok(ret);
        }
        let doub_block_num = doub_indir_ptr as usize - self.block_offset;
        let data = self
            .read_dir_doubly_ptr(doub_block_num)
            .expect("error reading doubly indirect pointer");
        ret.extend_from_slice(&data);

        // read triply indirect pointer
        let triply_indir_ptr = root.triply_indirect;
        if triply_indir_ptr == 0 {
            return Ok(ret);
        }
        let triply_block_num = triply_indir_ptr as usize - self.block_offset;
        let data = self
            .read_dir_triply_ptr(triply_block_num)
            .expect("error reading triply indirect pointer");
        ret.extend_from_slice(&data);

        Ok(ret)
    }

    // A helper function for `read_file_inode` to read the indirect pointer and return the data as a Vec<u8>
    fn read_file_indir_ptr(&self, block_num: usize) -> std::io::Result<Vec<u8>> {
        // indirect pointer points to a block full of direct block numbers/addresses
        // block addresses/numbers stored in the block are all 32-bit
        let indir_block = self.blocks[block_num];
        // entry_ptr points to the head of the indirect block
        let entry_ptr = indir_block.as_ptr();
        // byte_offset is the offset in bytes from the head of the indirect block, like the index of an array
        let mut byte_offset: isize = 0;
        let mut ret = Vec::new();
        while byte_offset < self.block_size as isize {
            // get direct block number from indirect ptr one at a time
            let dir_block_num = unsafe { *(entry_ptr.offset(byte_offset) as *const u32) };
            if dir_block_num == 0 {
                return Ok(ret);
            }
            let data = self.blocks[dir_block_num as usize];
            ret.extend_from_slice(data);
            // since the block number is 32-bit, we increment by 4 bytes
            byte_offset += 4;
        }
        Ok(ret)
    }

    // A helper function for `read_file_inode` read the doubly indirect pointer and return the data as a Vec<u8>
    fn read_file_doubly_ptr(&self, block_num: usize) -> std::io::Result<Vec<u8>> {
        // stores a bunch of singly indirect pointer block numbers
        let doub_block = self.blocks[block_num];
        let entry_ptr = doub_block.as_ptr();
        let mut byte_offset: isize = 0;
        let mut ret = Vec::new();
        while byte_offset < self.block_size as isize {
            let indir_block_num = unsafe { *(entry_ptr.offset(byte_offset) as *const u32) };
            if indir_block_num == 0 {
                return Ok(ret);
            }
            let data_from_indir = &(self.read_file_indir_ptr(indir_block_num as usize))
                .expect("error reading indirect pointer");
            ret.extend_from_slice(data_from_indir);
            byte_offset += 4;
        }
        Ok(ret)
    }

    // A helper function for `read_file_inode` read the triply indirect pointer and return the data as a Vec<u8>
    fn read_file_triply_ptr(&self, block_num: usize) -> std::io::Result<Vec<u8>> {
        let triply_indir_block = self.blocks[block_num];
        let entry_ptr = triply_indir_block.as_ptr();
        let mut byte_offset: isize = 0;
        let mut ret = Vec::new();
        while byte_offset < self.block_size as isize {
            let doub_indir_block_num = unsafe { *(entry_ptr.offset(byte_offset) as *const u32) };
            if doub_indir_block_num == 0 {
                return Ok(ret);
            }
            let data_from_doubly = &(self.read_file_doubly_ptr(doub_indir_block_num as usize))
                .expect("error reading doubly indirect pointer");
            ret.extend_from_slice(data_from_doubly);
            byte_offset += 4;
        }
        Ok(ret)
    }

    // given a (1-indexed) inode number, return the contents of that file
    pub fn read_file_inode(&self, inode: usize) -> std::io::Result<Vec<u8>> {
        // root is the inode we want to read
        let root = self.get_inode(inode);
        // traverse the direct pointers and get the data
        let mut ret = Vec::new();
        // iterate over all the direct pointers
        for direct_ptr in root.direct_pointer.iter() {
            // <- todo, support large directories
            // if block_num is 0, there are no more blocks -- invalid
            let block_num = *direct_ptr;
            if block_num == 0 {
                return Ok(ret);
            }
            // get the data from the block
            // direct pointers store block numbers
            // self.blocks[block_number] gives us the data in bytes
            let data = self.blocks[block_num as usize - self.block_offset];
            ret.extend_from_slice(data);
        }

        // read indirect pointer
        let indirect_ptr = root.indirect_pointer;
        if indirect_ptr == 0 {
            return Ok(ret);
        }
        let indir_block_num = indirect_ptr as usize - self.block_offset;
        let data = self
            .read_file_indir_ptr(indir_block_num)
            .expect("error reading indirect pointer");
        ret.extend_from_slice(&data);

        // read doubly indirect pointer
        let doub_indir_ptr = root.doubly_indirect;
        if doub_indir_ptr == 0 {
            return Ok(ret);
        }
        let doub_block_num = doub_indir_ptr as usize - self.block_offset;
        let data = self
            .read_file_doubly_ptr(doub_block_num)
            .expect("error reading doubly indirect pointer");
        ret.extend_from_slice(&data);

        // read triply indirect pointer
        let triply_indir_ptr = root.triply_indirect;
        if triply_indir_ptr == 0 {
            return Ok(ret);
        }
        let triply_block_num = triply_indir_ptr as usize - self.block_offset;
        let data = self
            .read_file_triply_ptr(triply_block_num)
            .expect("error reading triply indirect pointer");
        ret.extend_from_slice(&data);

        Ok(ret)
    }
}

impl fmt::Debug for Inode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if self.size_low == 0 && self.size_high == 0 {
            f.debug_struct("").finish()
        } else {
            f.debug_struct("Inode")
                .field("type_perm", &self.type_perm)
                .field("size_low", &self.size_low)
                .field("direct_pointers", &self.direct_pointer)
                .field("indirect_pointer", &self.indirect_pointer)
                .finish()
        }
    }
}

fn main() -> Result<()> {
    let disk = include_bytes!("../myfsplusbeemovie.ext2");
    let start_addr: usize = disk.as_ptr() as usize;
    let ext2 = Ext2::new(&disk[..], start_addr);

    let mut current_working_inode: usize = 2; // 2 is the root inode

    let mut rl = DefaultEditor::new()?;
    loop {
        // fetch the children of the current working directory
        let dirs = match ext2.read_dir_inode(current_working_inode) {
            Ok(dir_listing) => {
                dir_listing // the result is a vector of (inode, name) tuples
            }
            Err(_) => {
                println!("unable to read cwd");
                break;
            }
        };

        let buffer = rl.readline(":> ");
        if let Ok(line) = buffer {
            if line.starts_with("ls") {
                // `ls` prints our cwd's children
                // TODO: support arguments to ls (print that directory's children instead)
                for dir in &dirs {
                    print!("{}\t", dir.1); //dir.1 is the name of the directory
                }
                println!();
            } else if line.starts_with("cd") {
                // `cd` with no arguments, cd goes back to root
                // `cd dir_name` moves cwd to that directory
                let elts: Vec<&str> = line.split(' ').collect();
                if elts.len() == 1 {
                    // go back to root
                    current_working_inode = 2;
                } else {
                    // TODO: if the argument is a path, follow the path
                    // e.g., cd dir_1/dir_2 should move you down 2 directories
                    // deeper into dir_2
                    let to_dir = elts[1];
                    let mut found = false;
                    for dir in &dirs {
                        if dir.1.to_string().eq(to_dir) {
                            // TODO: maybe don't just assume this is a directory
                            // if the inode is not a dir, print an error
                            if (ext2.get_inode(dir.0).type_perm & structs::TypePerm::DIRECTORY)
                                == structs::TypePerm::DIRECTORY
                            {
                                found = true;
                                current_working_inode = dir.0;
                            } else {
                                found = true;
                                println!("cd: not a directory: {}", dir.1);
                            }
                        }
                    }
                    if !found {
                        println!("unable to locate {}, cwd unchanged", to_dir);
                    }
                }
            } else if line.starts_with("mkdir") {
                // `mkdir childname`
                // consider supporting `-p path/to_file` to create a path of directories
                let elts: Vec<&str> = line.split(' ').collect();
                // check valid argument
                if elts.len() != 2 {
                    println!("usage: mkdir dirname");
                    continue;
                }
                let dirname = elts[1];
                // check directory name unique in cwd
                for dir in &dirs {
                    // dir.0 is inode number
                    // dir.1 is the name of the directory
                    // 
                    if dir.1.to_string() == dirname
                        && (ext2.get_inode(dir.0).type_perm & structs::TypePerm::DIRECTORY)
                            == structs::TypePerm::DIRECTORY
                    {
                        println!("directory name already exists in cwd");
                        continue;
                    }
                }
                // check if at least one unallocated inode in the whole filesystem
                if ext2.superblock.free_inodes_count < 1 {
                    println!("no unallocated inodes available");
                    continue;
                }
                // find the first block group with an unallocated inode
                // block_groups is an array of BlockGroupDescriptors
                let mut group_idx = 0;
                for i in 0..ext2.block_groups.len() {
                    if ext2.block_groups[i].free_inodes_count > 0 {
                        group_idx = i;
                        break;
                    }
                }
                let mut block_group = &mut ext2.block_groups[group_idx];

                // find the first unallocated inode in that block group by using the inode usage bitmap of the block group
                // inode_usage_addr is the block address of inode usage bitmap

                let inode_usage_bitmap = ext2.blocks[block_group.inode_usage_addr as usize];
                println!("inode_usage_bitmap: {:?}", inode_usage_bitmap); // this line prints out the bitmap for debugging purposes
                println!("inode_usage_bitmap length: {:?}", inode_usage_bitmap.len()); // this line prints out the bitmap length for debugging purposes

                // Read bitmap, figure out the first unallocated inode
                // Each byte represents the allocation status of 8 inodes
                // For each byte, use bitwise operations to check allocation status of inode bit
                // if bit is 0 --> inode is unallocated
                // if bit is 1 --> the inode is allocated
                // should read the bitmap from back to front
                
                let mut first_unallocated_inode;
                // read bitmap from the back
                // we have 2 block groups, each with an inode usage bitmap
                // each inode usage bitmap has a length of 1024 which can represent 1024*8 inodes
                // 
                // we only have 2560 inodes, so space is wasted
                // 2560/8 = only 320 bytes needed to represent all inodes in filesystem
                for i in (0..inode_usage_bitmap.len()).rev() {
                    // inode is 1-indexed
                    const MASK: u8 = 1;
                    let len = inode_usage_bitmap.len();
                    for bit in 1..9 {
                        // check if inode is unallocated
                        if (inode_usage_bitmap[i] & (MASK << (bit - 1))) == 0 {
                            println!("{}", inode_usage_bitmap[i]);
                            println!("{}", MASK << (bit - 1));
                            // inode is unallocated
                            // inode number is 1-indexed
                            first_unallocated_inode = ((len - i) * 8) + bit;
                            break;
                        }
                    }
                }

                // Create DirectoryEntry
                // let mut new_dir = structs::DirectoryEntry {
                //     inode: first_unallocated_inode as u32,
                //     entry_size: 123,
                //     name_length: dirname.len() as u8,
                //     type_indicator: structs::TypeIndicator::Directory,
                //     name: NulStr::from(dirname).unwrap(),
                // };
                

                // Update block group information
                block_group.free_inodes_count -= 1;
                block_group.dirs_count += 1;

                // allocate an inode
                // create a directory with the given name, add a link to cwd
                    // current_working_inode

            } else if line.starts_with("cat") {
                // `cat filename`
                // print the contents of filename to stdout
                // if it's a directory, print a nice error
                // get the arguments
                let elts: Vec<&str> = line.split(' ').collect();
                if elts.len() != 2 {
                    println!("usage: cat filename");
                    continue;
                }
                let filename = elts[1];
                // check if the file exists
                let mut found = false;
                for dir in &dirs {
                    // if the file exists, print it
                    if dir.1.to_string().eq(filename) {
                        found = !found;
                        let inode = ext2.get_inode(dir.0);
                        // if the inode is a directory, print an error
                        if (inode.type_perm & structs::TypePerm::DIRECTORY)
                            == structs::TypePerm::DIRECTORY
                        {
                            println!("cat: {}: Is a directory", filename);
                        } else {
                            // print the contents of the file
                            let content = ext2.read_file_inode(dir.0);
                            match content {
                                Ok(content) => {
                                    io::stdout().write_all(&content).unwrap();
                                }
                                Err(_) => {
                                    println!("cat: {}: No such file or directory", filename);
                                }
                            }
                        }
                    }
                }
                // if not found, print an error
                if !found {
                    println!("cat: {}: No such file or directory", filename);
                }
            } else if line.starts_with("rm") {
                // `rm target`
                // unlink a file or empty directory
                println!("rm not yet implemented");
            } else if line.starts_with("mount") {
                // `mount host_filename mountpoint`
                // mount an ext2 filesystem over an existing empty directory
                println!("mount not yet implemented");
            } else if line.starts_with("link") {
                // `link arg_1 arg_2`
                // create a hard link from arg_1 to arg_2
                // consider what to do if arg2 does- or does-not end in "/"
                // and/or if arg2 is an existing directory name
                println!("link not yet implemented");
            } else if line.starts_with("quit") || line.starts_with("exit") {
                break;
            }
        } else {
            println!("bye!");
            break;
        }
    }
    Ok(())
}
