# Austin and Yifang CSCI 393 Ext2 Filesystem Final Project

## Project goal

 Our project goal is to extend the Ext2 Filesystem functions starting from the provided code from the CSCI393 course.
 
 `cargo run` will start a session that looks like a shell. 

 Here's an example session:
```
% cargo run
   <building and intro stuff>
:> ls
.	..	lost+found	test_directory	hello.txt	
:> cat hello.txt
cat not yet implemented
:> cd test_directory
:> ls
.	..	file_in_folder.txt	
:> cd file_in_folder.txt    # <- whoops
:> ls
'm a file inside a folder.  # <- whoops^2
	
:> 
```

## Implementation (our results)

### **TO-DO** Return raw bytes ... (Explain this)

**TO-DO**: (Explain `read_file_inode` returns raw bytes instead of string to account for a UTF character (2 bytes) possibly being stored in two different blocks for `cat`)

### Supporting large files for `cat`

Implementation for supporting for large files for `cat`:
- create a method `read_file_inode` that traverses the direct pointers of the given inode. 
- each of these direct pointers stores a block number that can used to index into the Ext2 filesystem's `blocks` vector to attain the data stored in bytes. 
- if a block number is equal to `0`, it is invalid and there are no more blocks to read, so our function returns the data. 
- if the given file inode requires more space, we use the singly indirect pointer, then the doubly indirect pointer, and lastly the triply indirect pointer by calling the `read_file_indir_ptr`, `read_file_doubly_ptr`, and `read_file_triply_ptr` methods, respectively. 
- `read_file_indir_ptr` accesses the indirect pointer block from the block number given and an entry pointer is assigned to point to the head of this indirect block. To read this entire block, we iterate with a byte offset of 4 since the block is an array of `u8` and we want to read every 4 bytes in order to attain the direct block numbers stored in the indirect block as `u32`. `read_file_doubly_ptr` and `read_file_triply_ptr` behave similarly, calling the pointer's function below them, using each of their stored block numbers as input and extending the vector of bytes we are returning.

### Supporting large directories for `ls` and `cd`

The logic for implementing large directory support for `ls` and `cd` is very similar. We implemented a method `read_dir_inode` that almost mimics `read_file_inode`, but we are iterating with the byte offset of the directory's entry size instead. Therefore, we're not just reading the whole direct pointer block's data at a time. 

## What we learned

Overall, we have learned a lot about how pointers work within filesystems and how data gets stored. We learned that these pointers, even direct pointers, point to block numbers instead of actual data. These block numbers are used to attain the data from the filesystem blocks.

## Interesting Next Steps

- Write tests 
  - Currently, the `beemovie.txt` file in the `myfsplusbeemovie.ext2` disk image file can only test our `cat` function up to singly indirect pointers 
  - large directories for `ls` and `cd` do not have a disk image file for testing; currently, only direct pointers are being used when calling these functions 
- Fix `read_fir_inode`
  - since we're reading block by block but the entry size of a directory is not fixed, its data may be stored in two different blocks
- Handle reading sparse files
  - Our code currently reads and appends data block by block, starting from direct pointer blocks. So, we stop reading a file as soon as we encounter a `0` in a block. 
- writing sparse files
  - `seek` function

Credits: Reed College CS393 students, @tzlil on the Rust #osdev discord