use std::fs::File;
use std::io::{self, Read, Seek, SeekFrom};
use std::path::Path;

use bytes::Bytes;
use memmap2::Mmap;

use crate::error::SparseError;

pub const SPARSE_HEADER_MAGIC: u32 = 0xED26FF3A;
pub const SPARSE_MAJOR_VERSION: u16 = 1;
pub const SPARSE_HEADER_SIZE: usize = 28;
pub const CHUNK_HEADER_SIZE: usize = 12;

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChunkType {
    Raw = 0xCAC1,
    Fill = 0xCAC2,
    DontCare = 0xCAC3,
    Crc32 = 0xCAC4,
}

impl ChunkType {
    pub fn from_u16(value: u16) -> Result<Self, SparseError> {
        match value {
            0xCAC1 => Ok(ChunkType::Raw),
            0xCAC2 => Ok(ChunkType::Fill),
            0xCAC3 => Ok(ChunkType::DontCare),
            0xCAC4 => Ok(ChunkType::Crc32),
            _ => Err(SparseError::InvalidChunkType(value)),
        }
    }
}
#[derive(Debug, Clone, Copy)]
pub struct SparseHeader {
    pub magic: u32,
    pub major_version: u16,
    pub minor_version: u16,
    pub file_hdr_sz: u16,
    pub chunk_hdr_sz: u16,
    pub blk_sz: u32,
    pub total_blks: u32,
    pub total_chunks: u32,
    pub image_checksum: u32,
}

impl SparseHeader {
    pub fn from_bytes(data: &[u8]) -> Result<Self, SparseError> {
        if data.len() < SPARSE_HEADER_SIZE {
            return Err(SparseError::InvalidHeader("数据太短".into()));
        }
        let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
        let major_version = u16::from_le_bytes([data[4], data[5]]);
        let minor_version = u16::from_le_bytes([data[6], data[7]]);
        let file_hdr_sz = u16::from_le_bytes([data[8], data[9]]);
        let chunk_hdr_sz = u16::from_le_bytes([data[10], data[11]]);
        let blk_sz = u32::from_le_bytes([data[12], data[13], data[14], data[15]]);
        let total_blks = u32::from_le_bytes([data[16], data[17], data[18], data[19]]);
        let total_chunks = u32::from_le_bytes([data[20], data[21], data[22], data[23]]);
        let image_checksum = u32::from_le_bytes([data[24], data[25], data[26], data[27]]);

        Ok(Self {
            magic,
            major_version,
            minor_version,
            file_hdr_sz,
            chunk_hdr_sz,
            blk_sz,
            total_blks,
            total_chunks,
            image_checksum,
        })
    }
    pub fn validate(&self) -> Result<(), SparseError> {
        if self.magic != SPARSE_HEADER_MAGIC {
            return Err(SparseError::InvalidMagic(self.magic));
        }
        if self.major_version != SPARSE_MAJOR_VERSION {
            return Err(SparseError::UnsupportedVersion(
                self.major_version,
                self.minor_version,
            ));
        }
        if self.blk_sz == 0 {
            return Err(SparseError::InvalidHeader("块大小不能为 0".into()));
        }
        Ok(())
    }
    pub fn expanded_size(&self) -> u64 {
        self.blk_sz as u64 * self.total_blks as u64
    }
}
#[derive(Debug, Clone, Copy)]
pub struct ChunkHeader {
    pub chunk_type: u16,
    pub reserved1: u16,
    pub chunk_sz: u32,
    pub total_sz: u32,
}

impl ChunkHeader {
    pub fn from_bytes(data: &[u8]) -> Result<Self, SparseError> {
        if data.len() < CHUNK_HEADER_SIZE {
            return Err(SparseError::InvalidHeader("Chunk 头数据太短".into()));
        }

        let chunk_type = u16::from_le_bytes([data[0], data[1]]);
        let reserved1 = u16::from_le_bytes([data[2], data[3]]);
        let chunk_sz = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
        let total_sz = u32::from_le_bytes([data[8], data[9], data[10], data[11]]);

        Ok(Self {
            chunk_type,
            reserved1,
            chunk_sz,
            total_sz,
        })
    }
    pub fn get_type(&self) -> Result<ChunkType, SparseError> {
        ChunkType::from_u16(self.chunk_type)
    }
    pub fn data_size(&self) -> u32 {
        self.total_sz.saturating_sub(CHUNK_HEADER_SIZE as u32)
    }
}
#[derive(Debug, Clone)]
pub struct ChunkInfo {
    pub chunk_type: ChunkType,
    pub output_blocks: u32,
    pub data_offset: u64,
    pub data_size: u32,
}

impl ChunkInfo {
    pub fn output_size(&self, block_size: u32) -> u64 {
        self.output_blocks as u64 * block_size as u64
    }
}
pub struct MappedSparseFile {
    mmap: Mmap,
    header: SparseHeader,
    chunks: Vec<ChunkInfo>,
}

impl MappedSparseFile {
    pub fn open(path: &Path) -> Result<Self, SparseError> {
        let file = File::open(path).map_err(SparseError::Io)?;
        Self::from_file(&file)
    }
    pub fn from_file(file: &File) -> Result<Self, SparseError> {
        let mmap = unsafe { Mmap::map(file).map_err(SparseError::Io)? };
        Self::from_mmap(mmap)
    }
    pub fn from_mmap(mmap: Mmap) -> Result<Self, SparseError> {
        if mmap.len() < SPARSE_HEADER_SIZE {
            return Err(SparseError::InvalidHeader("文件太小".into()));
        }
        let header = SparseHeader::from_bytes(&mmap[..SPARSE_HEADER_SIZE])?;
        header.validate()?;
        let chunks = Self::parse_chunks(&mmap, &header)?;

        Ok(Self {
            mmap,
            header,
            chunks,
        })
    }
    fn parse_chunks(mmap: &Mmap, header: &SparseHeader) -> Result<Vec<ChunkInfo>, SparseError> {
        let mut chunks = Vec::with_capacity(header.total_chunks as usize);
        let mut offset = header.file_hdr_sz as u64;

        for i in 0..header.total_chunks {
            if offset as usize + CHUNK_HEADER_SIZE > mmap.len() {
                return Err(SparseError::InvalidHeader(format!(
                    "Chunk {} 超出文件范围",
                    i
                )));
            }

            let chunk_header = ChunkHeader::from_bytes(
                &mmap[offset as usize..offset as usize + CHUNK_HEADER_SIZE],
            )?;

            let chunk_type = chunk_header.get_type()?;
            let data_offset = offset + CHUNK_HEADER_SIZE as u64;
            let data_size = chunk_header.data_size();

            chunks.push(ChunkInfo {
                chunk_type,
                output_blocks: chunk_header.chunk_sz,
                data_offset,
                data_size,
            });

            offset += chunk_header.total_sz as u64;
        }

        Ok(chunks)
    }
    pub fn header(&self) -> &SparseHeader {
        &self.header
    }
    pub fn chunks(&self) -> &[ChunkInfo] {
        &self.chunks
    }
    pub fn expanded_size(&self) -> u64 {
        self.header.expanded_size()
    }
    pub fn block_size(&self) -> u32 {
        self.header.blk_sz
    }

    pub fn chunk_data(&self, chunk_index: usize) -> Option<&[u8]> {
        let chunk = self.chunks.get(chunk_index)?;
        let start = chunk.data_offset as usize;
        let end = start + chunk.data_size as usize;

        if end > self.mmap.len() {
            return None;
        }

        Some(&self.mmap[start..end])
    }

    pub fn chunk_data_owned(&self, chunk_index: usize) -> Option<Bytes> {
        self.chunk_data(chunk_index)
            .map(|data| Bytes::copy_from_slice(data))
    }
    pub fn iter_chunks(&self) -> impl Iterator<Item = (ChunkType, &[u8])> + '_ {
        self.chunks
            .iter()
            .enumerate()
            .filter_map(|(i, chunk)| self.chunk_data(i).map(|data| (chunk.chunk_type, data)))
    }
}

pub struct StreamingSparseFile<R: Read + Seek> {
    reader: R,
    header: SparseHeader,
    current_chunk: u32,
    current_offset: u64,
}

impl<R: Read + Seek> StreamingSparseFile<R> {
    pub fn new(mut reader: R) -> Result<Self, SparseError> {
        let mut header_buf = [0u8; SPARSE_HEADER_SIZE];
        reader
            .read_exact(&mut header_buf)
            .map_err(SparseError::Io)?;

        let header = SparseHeader::from_bytes(&header_buf)?;
        header.validate()?;

        let current_offset = header.file_hdr_sz as u64;

        Ok(Self {
            reader,
            header,
            current_chunk: 0,
            current_offset,
        })
    }
    pub fn header(&self) -> &SparseHeader {
        &self.header
    }
    pub fn next_chunk(&mut self) -> Result<Option<(ChunkInfo, Vec<u8>)>, SparseError> {
        if self.current_chunk >= self.header.total_chunks {
            return Ok(None);
        }
        self.reader
            .seek(SeekFrom::Start(self.current_offset))
            .map_err(SparseError::Io)?;
        let mut chunk_buf = [0u8; CHUNK_HEADER_SIZE];
        self.reader
            .read_exact(&mut chunk_buf)
            .map_err(SparseError::Io)?;

        let chunk_header = ChunkHeader::from_bytes(&chunk_buf)?;
        let chunk_type = chunk_header.get_type()?;
        let data_size = chunk_header.data_size();
        let mut data = vec![0u8; data_size as usize];
        self.reader.read_exact(&mut data).map_err(SparseError::Io)?;

        let chunk_info = ChunkInfo {
            chunk_type,
            output_blocks: chunk_header.chunk_sz,
            data_offset: self.current_offset + CHUNK_HEADER_SIZE as u64,
            data_size,
        };
        self.current_offset += chunk_header.total_sz as u64;
        self.current_chunk += 1;

        Ok(Some((chunk_info, data)))
    }
    pub fn reset(&mut self) -> Result<(), SparseError> {
        self.current_chunk = 0;
        self.current_offset = self.header.file_hdr_sz as u64;
        self.reader
            .seek(SeekFrom::Start(self.current_offset))
            .map_err(SparseError::Io)?;
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub struct ChunkMeta {
    pub chunk_type: ChunkType,
    pub output_blocks: u32,
    pub file_offset: u64,
    pub data_size: u32,
    pub start_block: u32,
}

pub struct StreamingResparse {
    file_path: std::path::PathBuf,
    header: SparseHeader,
    chunk_metas: Vec<ChunkMeta>,
    max_fragment_size: u64,
    current_fragment: usize,
    total_fragments: usize,
    fragment_ranges: Vec<FragmentRange>,
}
#[derive(Debug, Clone)]
pub struct FragmentRange {
    pub start_chunk: usize,
    pub end_chunk: usize,
    pub start_block: u32,
    pub end_block: u32,
    pub first_chunk_offset: usize,
    pub last_chunk_size: usize,
}

impl StreamingResparse {
    pub fn new(path: &std::path::Path, max_fragment_size: u64) -> Result<Self, SparseError> {
        let mut file = File::open(path).map_err(SparseError::Io)?;
        let mut header_buf = [0u8; SPARSE_HEADER_SIZE];
        file.read_exact(&mut header_buf).map_err(SparseError::Io)?;

        let header = SparseHeader::from_bytes(&header_buf)?;
        header.validate()?;
        let mut chunk_metas = Vec::with_capacity(header.total_chunks as usize);
        let mut offset = header.file_hdr_sz as u64;
        let mut current_block = 0u32;

        for _ in 0..header.total_chunks {
            file.seek(SeekFrom::Start(offset))
                .map_err(SparseError::Io)?;

            let mut chunk_buf = [0u8; CHUNK_HEADER_SIZE];
            file.read_exact(&mut chunk_buf).map_err(SparseError::Io)?;

            let chunk_header = ChunkHeader::from_bytes(&chunk_buf)?;
            let chunk_type = chunk_header.get_type()?;
            let data_size = chunk_header.data_size();

            chunk_metas.push(ChunkMeta {
                chunk_type,
                output_blocks: chunk_header.chunk_sz,
                file_offset: offset + CHUNK_HEADER_SIZE as u64,
                data_size,
                start_block: current_block,
            });

            current_block += chunk_header.chunk_sz;
            offset += chunk_header.total_sz as u64;
        }
        let fragment_ranges =
            Self::calculate_fragment_ranges(&chunk_metas, &header, max_fragment_size);

        let total_fragments = fragment_ranges.len();

        Ok(Self {
            file_path: path.to_path_buf(),
            header,
            chunk_metas,
            max_fragment_size,
            current_fragment: 0,
            total_fragments,
            fragment_ranges,
        })
    }
    fn calculate_fragment_ranges(
        chunk_metas: &[ChunkMeta],
        header: &SparseHeader,
        max_size: u64,
    ) -> Vec<FragmentRange> {
        let block_size = header.blk_sz;
        let max_overhead = SPARSE_HEADER_SIZE + 4 * CHUNK_HEADER_SIZE;
        let effective_max = max_size.saturating_sub(max_overhead as u64);

        let mut ranges = Vec::new();

        let mut current_chunk_idx = 0;
        let mut offset_in_current_chunk = 0usize;
        let mut current_block = 0u32;

        while current_chunk_idx < chunk_metas.len() {
            let start_chunk = current_chunk_idx;
            let start_block = current_block;
            let first_chunk_offset = offset_in_current_chunk;

            let mut current_transfer_size = 0u64;
            let mut end_block = start_block;
            let mut peek_idx = current_chunk_idx;
            let mut last_chunk_size_in_fragment = 0usize;

            while peek_idx < chunk_metas.len() {
                let meta = &chunk_metas[peek_idx];
                let is_first_in_frag = peek_idx == start_chunk;

                let data_bytes_left = if is_first_in_frag && meta.chunk_type == ChunkType::Raw {
                    meta.data_size as usize - first_chunk_offset
                } else {
                    meta.data_size as usize
                };

                let blocks_left = if is_first_in_frag {
                    meta.output_blocks - (first_chunk_offset / block_size as usize) as u32
                } else {
                    meta.output_blocks
                };

                let chunk_overhead = match meta.chunk_type {
                    ChunkType::Raw => CHUNK_HEADER_SIZE as u64,
                    ChunkType::Fill | ChunkType::Crc32 => CHUNK_HEADER_SIZE as u64 + 4,
                    ChunkType::DontCare => CHUNK_HEADER_SIZE as u64,
                };

                let required_transfer = chunk_overhead
                    + match meta.chunk_type {
                        ChunkType::Raw => data_bytes_left as u64,
                        _ => 0,
                    };

                if current_transfer_size + required_transfer > effective_max {
                    if peek_idx > start_chunk {
                        break;
                    }

                    if meta.chunk_type == ChunkType::Raw {
                        let available_transfer = (effective_max - current_transfer_size)
                            .saturating_sub(CHUNK_HEADER_SIZE as u64)
                            as usize;
                        let aligned_take_bytes =
                            (available_transfer / block_size as usize) * block_size as usize;

                        if aligned_take_bytes > 0 {
                            last_chunk_size_in_fragment = aligned_take_bytes;
                            end_block += (aligned_take_bytes / block_size as usize) as u32;
                            peek_idx += 1;
                            break;
                        } else {
                            last_chunk_size_in_fragment = block_size as usize;
                            end_block += 1;
                            peek_idx += 1;
                            break;
                        }
                    } else {
                        end_block += blocks_left;
                        peek_idx += 1;
                        break;
                    }
                }

                current_transfer_size += required_transfer;
                end_block += blocks_left;
                last_chunk_size_in_fragment = data_bytes_left;
                peek_idx += 1;
            }

            ranges.push(FragmentRange {
                start_chunk,
                end_chunk: peek_idx,
                start_block,
                end_block,
                first_chunk_offset,
                last_chunk_size: last_chunk_size_in_fragment,
            });

            let last_processed_idx = peek_idx - 1;
            let meta = &chunk_metas[last_processed_idx];

            let total_data_in_last_chunk = if last_processed_idx == start_chunk {
                meta.data_size as usize - first_chunk_offset
            } else {
                meta.data_size as usize
            };

            if meta.chunk_type == ChunkType::Raw
                && last_chunk_size_in_fragment < total_data_in_last_chunk
            {
                current_chunk_idx = last_processed_idx;
                offset_in_current_chunk = if last_processed_idx == start_chunk {
                    first_chunk_offset + last_chunk_size_in_fragment
                } else {
                    last_chunk_size_in_fragment
                };
                current_block = end_block;
            } else {
                current_chunk_idx = peek_idx;
                offset_in_current_chunk = 0;
                current_block = end_block;
            }
        }

        ranges
    }
    pub fn total_fragments(&self) -> usize {
        self.total_fragments
    }
    pub fn header(&self) -> &SparseHeader {
        &self.header
    }
    pub fn file_path(&self) -> &std::path::Path {
        &self.file_path
    }
    pub fn fragment_ranges_clone(&self) -> Vec<FragmentRange> {
        self.fragment_ranges.clone()
    }
    pub fn chunk_metas_clone(&self) -> Vec<ChunkMeta> {
        self.chunk_metas.clone()
    }
    pub fn total_transfer_size(&self) -> u64 {
        let mut size = 0u64;
        for meta in &self.chunk_metas {
            size += CHUNK_HEADER_SIZE as u64;
            size += match meta.chunk_type {
                ChunkType::Raw => meta.data_size as u64,
                ChunkType::Fill | ChunkType::Crc32 => 4,
                ChunkType::DontCare => 0,
            };
        }
        size + SPARSE_HEADER_SIZE as u64
    }

    pub fn next_fragment(&mut self) -> Result<Option<(Vec<u8>, usize, bool)>, SparseError> {
        if self.current_fragment >= self.total_fragments {
            return Ok(None);
        }

        let range = &self.fragment_ranges[self.current_fragment];
        let fragment_index = self.current_fragment;
        let is_last = fragment_index + 1 >= self.total_fragments;
        let mut file = File::open(&self.file_path).map_err(SparseError::Io)?;
        let data = self.build_fragment(&mut file, range)?;

        self.current_fragment += 1;

        Ok(Some((data, fragment_index, is_last)))
    }
    fn build_fragment(
        &self,
        file: &mut File,
        range: &FragmentRange,
    ) -> Result<Vec<u8>, SparseError> {
        let block_size = self.header.blk_sz;
        let total_blks = self.header.total_blks;

        let start_block = range.start_block;
        let end_block = range.end_block;
        let trailing_blocks = total_blks.saturating_sub(end_block);

        let mut num_chunks = (range.end_chunk - range.start_chunk) as u32;
        if start_block > 0 {
            num_chunks += 1;
        }
        if trailing_blocks > 0 {
            num_chunks += 1;
        }

        let mut buffer =
            Vec::with_capacity((end_block - start_block) as usize * block_size as usize + 4096);

        buffer.extend_from_slice(&SPARSE_HEADER_MAGIC.to_le_bytes());
        buffer.extend_from_slice(&1u16.to_le_bytes());
        buffer.extend_from_slice(&0u16.to_le_bytes());
        buffer.extend_from_slice(&(SPARSE_HEADER_SIZE as u16).to_le_bytes());
        buffer.extend_from_slice(&(CHUNK_HEADER_SIZE as u16).to_le_bytes());
        buffer.extend_from_slice(&block_size.to_le_bytes());
        buffer.extend_from_slice(&total_blks.to_le_bytes());
        buffer.extend_from_slice(&num_chunks.to_le_bytes());
        buffer.extend_from_slice(&0u32.to_le_bytes());

        if start_block > 0 {
            buffer.extend_from_slice(&0xCAC3u16.to_le_bytes());
            buffer.extend_from_slice(&0u16.to_le_bytes());
            buffer.extend_from_slice(&start_block.to_le_bytes());
            buffer.extend_from_slice(&(CHUNK_HEADER_SIZE as u32).to_le_bytes());
        }

        for i in range.start_chunk..range.end_chunk {
            let meta = &self.chunk_metas[i];
            let is_first = i == range.start_chunk;
            let is_last = i == range.end_chunk - 1;

            match meta.chunk_type {
                ChunkType::Raw => {
                    let start_offset = if is_first {
                        range.first_chunk_offset
                    } else {
                        0
                    };
                    let take_bytes = if is_last {
                        range.last_chunk_size
                    } else {
                        meta.data_size as usize - start_offset
                    };

                    let output_blocks =
                        ((take_bytes + block_size as usize - 1) / block_size as usize) as u32;
                    let aligned_bytes = (output_blocks as usize) * (block_size as usize);
                    let total_sz = (CHUNK_HEADER_SIZE + aligned_bytes) as u32;

                    buffer.extend_from_slice(&0xCAC1u16.to_le_bytes());
                    buffer.extend_from_slice(&0u16.to_le_bytes());
                    buffer.extend_from_slice(&output_blocks.to_le_bytes());
                    buffer.extend_from_slice(&total_sz.to_le_bytes());

                    file.seek(SeekFrom::Start(meta.file_offset + start_offset as u64))
                        .map_err(SparseError::Io)?;

                    let old_len = buffer.len();
                    buffer.resize(old_len + take_bytes, 0);
                    file.read_exact(&mut buffer[old_len..])
                        .map_err(SparseError::Io)?;

                    if take_bytes < aligned_bytes {
                        buffer.extend(std::iter::repeat(0u8).take(aligned_bytes - take_bytes));
                    }
                }
                ChunkType::Fill => {
                    let total_sz = (CHUNK_HEADER_SIZE + 4) as u32;
                    buffer.extend_from_slice(&0xCAC2u16.to_le_bytes());
                    buffer.extend_from_slice(&0u16.to_le_bytes());
                    buffer.extend_from_slice(&meta.output_blocks.to_le_bytes());
                    buffer.extend_from_slice(&total_sz.to_le_bytes());

                    file.seek(SeekFrom::Start(meta.file_offset))
                        .map_err(SparseError::Io)?;
                    let mut fill_val = [0u8; 4];
                    file.read_exact(&mut fill_val).map_err(SparseError::Io)?;
                    buffer.extend_from_slice(&fill_val);
                }
                ChunkType::DontCare => {
                    buffer.extend_from_slice(&0xCAC3u16.to_le_bytes());
                    buffer.extend_from_slice(&0u16.to_le_bytes());
                    buffer.extend_from_slice(&meta.output_blocks.to_le_bytes());
                    buffer.extend_from_slice(&(CHUNK_HEADER_SIZE as u32).to_le_bytes());
                }
                ChunkType::Crc32 => {
                    let total_sz = (CHUNK_HEADER_SIZE + 4) as u32;
                    buffer.extend_from_slice(&0xCAC4u16.to_le_bytes());
                    buffer.extend_from_slice(&0u16.to_le_bytes());
                    buffer.extend_from_slice(&0u32.to_le_bytes());
                    buffer.extend_from_slice(&total_sz.to_le_bytes());

                    file.seek(SeekFrom::Start(meta.file_offset))
                        .map_err(SparseError::Io)?;
                    let mut crc_val = [0u8; 4];
                    file.read_exact(&mut crc_val).map_err(SparseError::Io)?;
                    buffer.extend_from_slice(&crc_val);
                }
            }
        }

        if trailing_blocks > 0 {
            buffer.extend_from_slice(&0xCAC3u16.to_le_bytes());
            buffer.extend_from_slice(&0u16.to_le_bytes());
            buffer.extend_from_slice(&trailing_blocks.to_le_bytes());
            buffer.extend_from_slice(&(CHUNK_HEADER_SIZE as u32).to_le_bytes());
        }

        Ok(buffer)
    }
    pub fn reset(&mut self) {
        self.current_fragment = 0;
    }
}

pub fn build_fragment_standalone(
    file_path: &std::path::Path,
    header: &SparseHeader,
    chunk_metas: &[ChunkMeta],
    range: &FragmentRange,
) -> Result<Vec<u8>, SparseError> {
    let mut file = File::open(file_path).map_err(SparseError::Io)?;

    let block_size = header.blk_sz;
    let total_blks = header.total_blks;

    let start_block = range.start_block;
    let end_block = range.end_block;
    let trailing_blocks = total_blks.saturating_sub(end_block);
    let mut num_chunks = (range.end_chunk - range.start_chunk) as u32;
    if start_block > 0 {
        num_chunks += 1;
    }
    if trailing_blocks > 0 {
        num_chunks += 1;
    }
    let mut estimated_size = SPARSE_HEADER_SIZE + (num_chunks as usize) * CHUNK_HEADER_SIZE;
    for i in range.start_chunk..range.end_chunk {
        let meta = &chunk_metas[i];
        estimated_size += match meta.chunk_type {
            ChunkType::Raw => {
                let is_first = i == range.start_chunk;
                let is_last = i == range.end_chunk - 1;

                if is_first && range.first_chunk_offset > 0 {
                    if is_last
                        && range.last_chunk_size
                            < meta.data_size as usize - range.first_chunk_offset
                    {
                        range.last_chunk_size
                    } else {
                        meta.data_size as usize - range.first_chunk_offset
                    }
                } else if is_last && range.last_chunk_size < meta.data_size as usize {
                    range.last_chunk_size
                } else {
                    meta.data_size as usize
                }
            }
            ChunkType::Fill | ChunkType::Crc32 => 4,
            ChunkType::DontCare => 0,
        };
    }

    let mut buffer = Vec::with_capacity(estimated_size);
    buffer.extend_from_slice(&SPARSE_HEADER_MAGIC.to_le_bytes());
    buffer.extend_from_slice(&1u16.to_le_bytes());
    buffer.extend_from_slice(&0u16.to_le_bytes());
    buffer.extend_from_slice(&(SPARSE_HEADER_SIZE as u16).to_le_bytes());
    buffer.extend_from_slice(&(CHUNK_HEADER_SIZE as u16).to_le_bytes());
    buffer.extend_from_slice(&block_size.to_le_bytes());
    buffer.extend_from_slice(&total_blks.to_le_bytes());
    buffer.extend_from_slice(&num_chunks.to_le_bytes());
    buffer.extend_from_slice(&0u32.to_le_bytes());

    if start_block > 0 {
        buffer.extend_from_slice(&0xCAC3u16.to_le_bytes());
        buffer.extend_from_slice(&0u16.to_le_bytes());
        buffer.extend_from_slice(&start_block.to_le_bytes());
        buffer.extend_from_slice(&(CHUNK_HEADER_SIZE as u32).to_le_bytes());
    }
    for i in range.start_chunk..range.end_chunk {
        let meta = &chunk_metas[i];

        match meta.chunk_type {
            ChunkType::Raw => {
                let is_first = i == range.start_chunk;
                let is_last = i == range.end_chunk - 1;

                let data_size = if is_first && range.first_chunk_offset > 0 {
                    if is_last
                        && range.last_chunk_size
                            < meta.data_size as usize - range.first_chunk_offset
                    {
                        range.last_chunk_size
                    } else {
                        meta.data_size as usize - range.first_chunk_offset
                    }
                } else if is_last && range.last_chunk_size < meta.data_size as usize {
                    range.last_chunk_size
                } else {
                    meta.data_size as usize
                };

                let output_blocks =
                    ((data_size + block_size as usize - 1) / block_size as usize) as u32;
                let aligned_size = (output_blocks as usize) * (block_size as usize);
                let total_sz = (CHUNK_HEADER_SIZE + aligned_size) as u32;

                buffer.extend_from_slice(&0xCAC1u16.to_le_bytes());
                buffer.extend_from_slice(&0u16.to_le_bytes());
                buffer.extend_from_slice(&output_blocks.to_le_bytes());
                buffer.extend_from_slice(&total_sz.to_le_bytes());

                let file_offset = if is_first && range.first_chunk_offset > 0 {
                    meta.file_offset + range.first_chunk_offset as u64
                } else {
                    meta.file_offset
                };
                file.seek(SeekFrom::Start(file_offset))
                    .map_err(SparseError::Io)?;
                let old_len = buffer.len();
                buffer.resize(old_len + data_size, 0);
                file.read_exact(&mut buffer[old_len..])
                    .map_err(SparseError::Io)?;
                if data_size < aligned_size {
                    buffer.extend(std::iter::repeat(0u8).take(aligned_size - data_size));
                }
            }
            ChunkType::Fill => {
                let total_sz = (CHUNK_HEADER_SIZE + 4) as u32;
                buffer.extend_from_slice(&0xCAC2u16.to_le_bytes());
                buffer.extend_from_slice(&0u16.to_le_bytes());
                buffer.extend_from_slice(&meta.output_blocks.to_le_bytes());
                buffer.extend_from_slice(&total_sz.to_le_bytes());
                file.seek(SeekFrom::Start(meta.file_offset))
                    .map_err(SparseError::Io)?;
                let mut fill_val = [0u8; 4];
                file.read_exact(&mut fill_val).map_err(SparseError::Io)?;
                buffer.extend_from_slice(&fill_val);
            }
            ChunkType::DontCare => {
                buffer.extend_from_slice(&0xCAC3u16.to_le_bytes());
                buffer.extend_from_slice(&0u16.to_le_bytes());
                buffer.extend_from_slice(&meta.output_blocks.to_le_bytes());
                buffer.extend_from_slice(&(CHUNK_HEADER_SIZE as u32).to_le_bytes());
            }
            ChunkType::Crc32 => {
                let total_sz = (CHUNK_HEADER_SIZE + 4) as u32;
                buffer.extend_from_slice(&0xCAC4u16.to_le_bytes());
                buffer.extend_from_slice(&0u16.to_le_bytes());
                buffer.extend_from_slice(&0u32.to_le_bytes());
                buffer.extend_from_slice(&total_sz.to_le_bytes());
                file.seek(SeekFrom::Start(meta.file_offset))
                    .map_err(SparseError::Io)?;
                let mut crc_val = [0u8; 4];
                file.read_exact(&mut crc_val).map_err(SparseError::Io)?;
                buffer.extend_from_slice(&crc_val);
            }
        }
    }
    if trailing_blocks > 0 {
        buffer.extend_from_slice(&0xCAC3u16.to_le_bytes());
        buffer.extend_from_slice(&0u16.to_le_bytes());
        buffer.extend_from_slice(&trailing_blocks.to_le_bytes());
        buffer.extend_from_slice(&(CHUNK_HEADER_SIZE as u32).to_le_bytes());
    }

    Ok(buffer)
}

pub struct SparseWriter {
    block_size: u32,
    total_size: u64,
    chunks: Vec<SparseChunk>,
    current_size: u64,
    current_blocks: u32,
}
#[derive(Clone, Debug)]
pub enum SparseChunk {
    Raw(Vec<u8>),
    Fill { value: u32, blocks: u32 },
    DontCare { blocks: u32 },
    Crc32(u32),
}

impl SparseChunk {
    pub fn transfer_size(&self) -> u64 {
        let data_size = match self {
            SparseChunk::Raw(data) => data.len() as u64,
            SparseChunk::Fill { .. } => 4,
            SparseChunk::DontCare { .. } => 0,
            SparseChunk::Crc32(_) => 4,
        };
        CHUNK_HEADER_SIZE as u64 + data_size
    }
    pub fn output_blocks(&self, block_size: u32) -> u32 {
        match self {
            SparseChunk::Raw(data) => (data.len() as u32 + block_size - 1) / block_size,
            SparseChunk::Fill { blocks, .. } => *blocks,
            SparseChunk::DontCare { blocks } => *blocks,
            SparseChunk::Crc32(_) => 0,
        }
    }
    pub fn chunk_type(&self) -> ChunkType {
        match self {
            SparseChunk::Raw(_) => ChunkType::Raw,
            SparseChunk::Fill { .. } => ChunkType::Fill,
            SparseChunk::DontCare { .. } => ChunkType::DontCare,
            SparseChunk::Crc32(_) => ChunkType::Crc32,
        }
    }
}

impl SparseWriter {
    pub fn new(block_size: u32, total_size: u64) -> Self {
        Self {
            block_size,
            total_size,
            chunks: Vec::new(),
            current_size: SPARSE_HEADER_SIZE as u64,
            current_blocks: 0,
        }
    }
    pub fn add_chunk(&mut self, chunk: SparseChunk) {
        let blocks = chunk.output_blocks(self.block_size);
        self.current_size += chunk.transfer_size();
        self.current_blocks += blocks;
        self.chunks.push(chunk);
    }
    pub fn transfer_size(&self) -> u64 {
        self.current_size
    }
    pub fn output_blocks(&self) -> u32 {
        self.current_blocks
    }
    pub fn build(&self) -> Vec<u8> {
        let mut output = Vec::with_capacity(self.current_size as usize);
        output.extend_from_slice(&SPARSE_HEADER_MAGIC.to_le_bytes());
        output.extend_from_slice(&1u16.to_le_bytes());
        output.extend_from_slice(&0u16.to_le_bytes());
        output.extend_from_slice(&(SPARSE_HEADER_SIZE as u16).to_le_bytes());
        output.extend_from_slice(&(CHUNK_HEADER_SIZE as u16).to_le_bytes());
        output.extend_from_slice(&self.block_size.to_le_bytes());
        output.extend_from_slice(&self.current_blocks.to_le_bytes());
        output.extend_from_slice(&(self.chunks.len() as u32).to_le_bytes());
        output.extend_from_slice(&0u32.to_le_bytes());

        for chunk in &self.chunks {
            let chunk_type = chunk.chunk_type() as u16;
            let output_blocks = chunk.output_blocks(self.block_size);
            let total_sz = chunk.transfer_size() as u32;
            output.extend_from_slice(&chunk_type.to_le_bytes());
            output.extend_from_slice(&0u16.to_le_bytes());
            output.extend_from_slice(&output_blocks.to_le_bytes());
            output.extend_from_slice(&total_sz.to_le_bytes());
            match chunk {
                SparseChunk::Raw(data) => output.extend_from_slice(data),
                SparseChunk::Fill { value, .. } => output.extend_from_slice(&value.to_le_bytes()),
                SparseChunk::DontCare { .. } => {}
                SparseChunk::Crc32(crc) => output.extend_from_slice(&crc.to_le_bytes()),
            }
        }

        output
    }
    pub fn clear(&mut self) {
        self.chunks.clear();
        self.current_size = SPARSE_HEADER_SIZE as u64;
        self.current_blocks = 0;
    }
}

#[derive(Debug)]
pub struct SparseFragment {
    pub data: Vec<u8>,
    pub index: usize,
    pub total: usize,
    pub transfer_size: u64,
    pub expanded_size: u64,
}

pub fn resparse(
    sparse: &MappedSparseFile,
    max_size: u64,
) -> Result<Vec<SparseFragment>, SparseError> {
    let header = sparse.header();
    let block_size = header.blk_sz;
    let total_blks = header.total_blks;
    let total_size = header.expanded_size();
    let file_transfer_size = calculate_transfer_size(sparse);
    if file_transfer_size <= max_size {
        return Ok(vec![SparseFragment {
            data: sparse.mmap[..].to_vec(),
            index: 0,
            total: 1,
            transfer_size: file_transfer_size,
            expanded_size: total_size,
        }]);
    }
    struct ChunkWithOffset {
        chunk: SparseChunk,
        start_block: u32,
        num_blocks: u32,
    }

    let mut all_chunks: Vec<ChunkWithOffset> = Vec::new();
    let mut current_block: u32 = 0;

    for (i, chunk_info) in sparse.chunks().iter().enumerate() {
        let chunk_data = sparse
            .chunk_data(i)
            .ok_or_else(|| SparseError::InvalidHeader(format!("无法读取 chunk {} 的数据", i)))?;

        let chunk = match chunk_info.chunk_type {
            ChunkType::Raw => SparseChunk::Raw(chunk_data.to_vec()),
            ChunkType::Fill => {
                let value = if chunk_data.len() >= 4 {
                    u32::from_le_bytes([chunk_data[0], chunk_data[1], chunk_data[2], chunk_data[3]])
                } else {
                    0
                };
                SparseChunk::Fill {
                    value,
                    blocks: chunk_info.output_blocks,
                }
            }
            ChunkType::DontCare => SparseChunk::DontCare {
                blocks: chunk_info.output_blocks,
            },
            ChunkType::Crc32 => {
                let crc = if chunk_data.len() >= 4 {
                    u32::from_le_bytes([chunk_data[0], chunk_data[1], chunk_data[2], chunk_data[3]])
                } else {
                    0
                };
                SparseChunk::Crc32(crc)
            }
        };

        let num_blocks = chunk.output_blocks(block_size);
        all_chunks.push(ChunkWithOffset {
            chunk,
            start_block: current_block,
            num_blocks,
        });
        current_block += num_blocks;
    }

    let max_overhead = SPARSE_HEADER_SIZE + 4 * CHUNK_HEADER_SIZE;
    let effective_max_size = max_size - max_overhead as u64;

    let mut fragments: Vec<SparseFragment> = Vec::new();
    let mut chunk_idx = 0;

    while chunk_idx < all_chunks.len() {
        let fragment_start_block = all_chunks[chunk_idx].start_block;
        let mut fragment_chunks: Vec<SparseChunk> = Vec::new();
        let mut fragment_transfer_size: u64 = 0;
        let mut fragment_end_block = fragment_start_block;
        while chunk_idx < all_chunks.len() {
            let chunk_with_offset = &all_chunks[chunk_idx];
            let chunk_transfer = chunk_with_offset.chunk.transfer_size();
            if fragment_transfer_size + chunk_transfer > effective_max_size
                && !fragment_chunks.is_empty()
            {
                break;
            }
            if let SparseChunk::Raw(ref data) = chunk_with_offset.chunk {
                let remaining_space = (effective_max_size - fragment_transfer_size) as usize;
                if remaining_space < CHUNK_HEADER_SIZE + block_size as usize
                    && !fragment_chunks.is_empty()
                {
                    break;
                }

                let max_data_size = remaining_space.saturating_sub(CHUNK_HEADER_SIZE);
                let aligned_max = (max_data_size / block_size as usize) * block_size as usize;

                if aligned_max > 0 && aligned_max < data.len() {
                    let take_data = data[..aligned_max].to_vec();
                    let take_blocks = (aligned_max / block_size as usize) as u32;

                    fragment_chunks.push(SparseChunk::Raw(take_data));
                    fragment_transfer_size += CHUNK_HEADER_SIZE as u64 + aligned_max as u64;
                    fragment_end_block += take_blocks;
                    let remaining_data = data[aligned_max..].to_vec();
                    let remaining_blocks = chunk_with_offset.num_blocks - take_blocks;
                    all_chunks[chunk_idx] = ChunkWithOffset {
                        chunk: SparseChunk::Raw(remaining_data),
                        start_block: fragment_end_block,
                        num_blocks: remaining_blocks,
                    };
                    break;
                }
            }
            fragment_chunks.push(chunk_with_offset.chunk.clone());
            fragment_transfer_size += chunk_transfer;
            fragment_end_block += chunk_with_offset.num_blocks;
            chunk_idx += 1;
        }

        if fragment_chunks.is_empty() {
            break;
        }
        let trailing_blocks = total_blks.saturating_sub(fragment_end_block);
        let mut num_chunks = fragment_chunks.len() as u32;
        if fragment_start_block > 0 {
            num_chunks += 1;
        }
        if trailing_blocks > 0 {
            num_chunks += 1;
        }
        let mut actual_transfer_size = SPARSE_HEADER_SIZE as u64;
        if fragment_start_block > 0 {
            actual_transfer_size += CHUNK_HEADER_SIZE as u64;
        }
        actual_transfer_size += fragment_transfer_size;
        if trailing_blocks > 0 {
            actual_transfer_size += CHUNK_HEADER_SIZE as u64;
        }
        let mut data = Vec::with_capacity(actual_transfer_size as usize);
        data.extend_from_slice(&SPARSE_HEADER_MAGIC.to_le_bytes());
        data.extend_from_slice(&1u16.to_le_bytes());
        data.extend_from_slice(&0u16.to_le_bytes());
        data.extend_from_slice(&(SPARSE_HEADER_SIZE as u16).to_le_bytes());
        data.extend_from_slice(&(CHUNK_HEADER_SIZE as u16).to_le_bytes());
        data.extend_from_slice(&block_size.to_le_bytes());
        data.extend_from_slice(&total_blks.to_le_bytes());
        data.extend_from_slice(&num_chunks.to_le_bytes());
        data.extend_from_slice(&0u32.to_le_bytes());

        if fragment_start_block > 0 {
            data.extend_from_slice(&0xCAC3u16.to_le_bytes());
            data.extend_from_slice(&0u16.to_le_bytes());
            data.extend_from_slice(&fragment_start_block.to_le_bytes());
            data.extend_from_slice(&(CHUNK_HEADER_SIZE as u32).to_le_bytes());
        }
        for chunk in &fragment_chunks {
            let chunk_type = chunk.chunk_type() as u16;
            let output_blocks = chunk.output_blocks(block_size);
            let total_sz = chunk.transfer_size() as u32;

            data.extend_from_slice(&chunk_type.to_le_bytes());
            data.extend_from_slice(&0u16.to_le_bytes());
            data.extend_from_slice(&output_blocks.to_le_bytes());
            data.extend_from_slice(&total_sz.to_le_bytes());

            match chunk {
                SparseChunk::Raw(raw_data) => data.extend_from_slice(raw_data),
                SparseChunk::Fill { value, .. } => data.extend_from_slice(&value.to_le_bytes()),
                SparseChunk::DontCare { .. } => {}
                SparseChunk::Crc32(crc) => data.extend_from_slice(&crc.to_le_bytes()),
            }
        }
        if trailing_blocks > 0 {
            data.extend_from_slice(&0xCAC3u16.to_le_bytes());
            data.extend_from_slice(&0u16.to_le_bytes());
            data.extend_from_slice(&trailing_blocks.to_le_bytes());
            data.extend_from_slice(&(CHUNK_HEADER_SIZE as u32).to_le_bytes());
        }

        let expanded = (fragment_end_block - fragment_start_block) as u64 * block_size as u64;
        fragments.push(SparseFragment {
            data,
            index: fragments.len(),
            total: 0,
            transfer_size: actual_transfer_size,
            expanded_size: expanded,
        });
    }
    let total = fragments.len();
    for frag in &mut fragments {
        frag.total = total;
    }

    Ok(fragments)
}
pub fn calculate_transfer_size(sparse: &MappedSparseFile) -> u64 {
    let mut size = SPARSE_HEADER_SIZE as u64;
    for chunk in sparse.chunks() {
        size += CHUNK_HEADER_SIZE as u64 + chunk.data_size as u64;
    }
    size
}

pub fn is_sparse_file(path: &Path) -> Result<bool, io::Error> {
    let mut file = File::open(path)?;
    let mut magic_buf = [0u8; 4];

    if file.read_exact(&mut magic_buf).is_err() {
        return Ok(false);
    }

    let magic = u32::from_le_bytes(magic_buf);
    Ok(magic == SPARSE_HEADER_MAGIC)
}
pub fn is_sparse_data(data: &[u8]) -> bool {
    if data.len() < 4 {
        return false;
    }
    let magic = u32::from_le_bytes([data[0], data[1], data[2], data[3]]);
    magic == SPARSE_HEADER_MAGIC
}
#[derive(Debug)]
pub struct SparseValidation {
    pub valid: bool,
    pub header: Option<SparseHeader>,
    pub file_size: u64,
    pub calculated_blocks: u32,
    pub calculated_transfer_size: u64,
    pub error: Option<String>,
}
pub fn validate_sparse_file(path: &Path) -> SparseValidation {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(e) => {
            return SparseValidation {
                valid: false,
                header: None,
                file_size: 0,
                calculated_blocks: 0,
                calculated_transfer_size: 0,
                error: Some(format!("无法打开文件: {}", e)),
            }
        }
    };

    let file_size = match file.metadata() {
        Ok(m) => m.len(),
        Err(e) => {
            return SparseValidation {
                valid: false,
                header: None,
                file_size: 0,
                calculated_blocks: 0,
                calculated_transfer_size: 0,
                error: Some(format!("无法获取文件大小: {}", e)),
            }
        }
    };
    if file_size < SPARSE_HEADER_SIZE as u64 {
        return SparseValidation {
            valid: false,
            header: None,
            file_size,
            calculated_blocks: 0,
            calculated_transfer_size: 0,
            error: Some(format!(
                "文件太小，无法包含 Sparse 头 (需要 {} 字节，实际 {} 字节)",
                SPARSE_HEADER_SIZE, file_size
            )),
        };
    }
    let mmap = match unsafe { Mmap::map(&file) } {
        Ok(m) => m,
        Err(e) => {
            return SparseValidation {
                valid: false,
                header: None,
                file_size,
                calculated_blocks: 0,
                calculated_transfer_size: 0,
                error: Some(format!("无法映射文件: {}", e)),
            }
        }
    };
    let header = match SparseHeader::from_bytes(&mmap[..SPARSE_HEADER_SIZE]) {
        Ok(h) => h,
        Err(e) => {
            return SparseValidation {
                valid: false,
                header: None,
                file_size,
                calculated_blocks: 0,
                calculated_transfer_size: 0,
                error: Some(format!("无法解析 Sparse 头: {}", e)),
            }
        }
    };
    if let Err(e) = header.validate() {
        return SparseValidation {
            valid: false,
            header: Some(header),
            file_size,
            calculated_blocks: 0,
            calculated_transfer_size: 0,
            error: Some(format!("Sparse 头验证失败: {}", e)),
        };
    }
    let mut offset = header.file_hdr_sz as u64;
    let mut calculated_blocks = 0u32;
    let mut calculated_transfer_size = SPARSE_HEADER_SIZE as u64;

    for i in 0..header.total_chunks {
        if offset as usize + CHUNK_HEADER_SIZE > mmap.len() {
            return SparseValidation {
                valid: false,
                header: Some(header),
                file_size,
                calculated_blocks,
                calculated_transfer_size,
                error: Some(format!(
                    "Chunk {} 头超出文件范围 (偏移 {}, 文件大小 {})",
                    i, offset, file_size
                )),
            };
        }
        let chunk_header = match ChunkHeader::from_bytes(
            &mmap[offset as usize..offset as usize + CHUNK_HEADER_SIZE],
        ) {
            Ok(h) => h,
            Err(e) => {
                return SparseValidation {
                    valid: false,
                    header: Some(header),
                    file_size,
                    calculated_blocks,
                    calculated_transfer_size,
                    error: Some(format!("Chunk {} 头解析失败: {}", i, e)),
                }
            }
        };
        if chunk_header.get_type().is_err() {
            return SparseValidation {
                valid: false,
                header: Some(header),
                file_size,
                calculated_blocks,
                calculated_transfer_size,
                error: Some(format!(
                    "Chunk {} 类型无效: 0x{:04X}",
                    i, chunk_header.chunk_type
                )),
            };
        }
        let chunk_end = offset + chunk_header.total_sz as u64;
        if chunk_end > file_size {
            return SparseValidation {
                valid: false,
                header: Some(header),
                file_size,
                calculated_blocks,
                calculated_transfer_size,
                error: Some(format!(
                    "Chunk {} 数据超出文件范围 (结束位置 {}, 文件大小 {})",
                    i, chunk_end, file_size
                )),
            };
        }

        calculated_blocks += chunk_header.chunk_sz;
        calculated_transfer_size += chunk_header.total_sz as u64;
        offset += chunk_header.total_sz as u64;
    }
    if calculated_blocks != header.total_blks {
        return SparseValidation {
            valid: false,
            header: Some(header),
            file_size,
            calculated_blocks,
            calculated_transfer_size,
            error: Some(format!(
                "总块数不匹配: 头部声明 {} 块，实际计算 {} 块",
                header.total_blks, calculated_blocks
            )),
        };
    }

    SparseValidation {
        valid: true,
        header: Some(header),
        file_size,
        calculated_blocks,
        calculated_transfer_size,
        error: None,
    }
}

pub fn quick_validate_sparse(path: &Path) -> Result<SparseHeader, SparseError> {
    let mut file = File::open(path).map_err(SparseError::Io)?;
    let file_size = file.metadata().map_err(SparseError::Io)?.len();
    let mut header_buf = [0u8; SPARSE_HEADER_SIZE];
    file.read_exact(&mut header_buf).map_err(SparseError::Io)?;

    let header = SparseHeader::from_bytes(&header_buf)?;
    header.validate()?;
    let min_size =
        header.file_hdr_sz as u64 + (header.total_chunks as u64 * CHUNK_HEADER_SIZE as u64);

    if file_size < min_size {
        return Err(SparseError::InvalidHeader(format!(
            "文件不完整: 最小需要 {} 字节，实际 {} 字节",
            min_size, file_size
        )));
    }

    Ok(header)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_chunk_type_from_u16() {
        assert_eq!(ChunkType::from_u16(0xCAC1).unwrap(), ChunkType::Raw);
        assert_eq!(ChunkType::from_u16(0xCAC2).unwrap(), ChunkType::Fill);
        assert_eq!(ChunkType::from_u16(0xCAC3).unwrap(), ChunkType::DontCare);
        assert_eq!(ChunkType::from_u16(0xCAC4).unwrap(), ChunkType::Crc32);
        assert!(ChunkType::from_u16(0x0000).is_err());
    }

    #[test]
    fn test_sparse_header_size() {
        assert_eq!(SPARSE_HEADER_SIZE, 28);
        assert_eq!(CHUNK_HEADER_SIZE, 12);
    }

    #[test]
    fn test_sparse_header_from_bytes() {
        let mut data = vec![0u8; SPARSE_HEADER_SIZE];
        data[0..4].copy_from_slice(&SPARSE_HEADER_MAGIC.to_le_bytes());
        data[4..6].copy_from_slice(&1u16.to_le_bytes());
        data[6..8].copy_from_slice(&0u16.to_le_bytes());
        data[8..10].copy_from_slice(&28u16.to_le_bytes());
        data[10..12].copy_from_slice(&12u16.to_le_bytes());
        data[12..16].copy_from_slice(&4096u32.to_le_bytes());
        data[16..20].copy_from_slice(&1000u32.to_le_bytes());
        data[20..24].copy_from_slice(&10u32.to_le_bytes());
        data[24..28].copy_from_slice(&0u32.to_le_bytes());

        let header = SparseHeader::from_bytes(&data).unwrap();
        assert!(header.validate().is_ok());
        assert_eq!(header.magic, SPARSE_HEADER_MAGIC);
        assert_eq!(header.major_version, 1);
        assert_eq!(header.blk_sz, 4096);
        assert_eq!(header.total_blks, 1000);
        assert_eq!(header.expanded_size(), 4096 * 1000);
    }

    #[test]
    fn test_sparse_header_validate() {
        let mut data = vec![0u8; SPARSE_HEADER_SIZE];
        data[0..4].copy_from_slice(&SPARSE_HEADER_MAGIC.to_le_bytes());
        data[4..6].copy_from_slice(&1u16.to_le_bytes());
        data[12..16].copy_from_slice(&4096u32.to_le_bytes());
        data[16..20].copy_from_slice(&100u32.to_le_bytes());

        let header = SparseHeader::from_bytes(&data).unwrap();
        assert!(header.validate().is_ok());
        let mut bad_magic = data.clone();
        bad_magic[0..4].copy_from_slice(&0x12345678u32.to_le_bytes());
        let header = SparseHeader::from_bytes(&bad_magic).unwrap();
        assert!(matches!(
            header.validate(),
            Err(SparseError::InvalidMagic(_))
        ));
        let mut bad_version = data.clone();
        bad_version[4..6].copy_from_slice(&2u16.to_le_bytes());
        let header = SparseHeader::from_bytes(&bad_version).unwrap();
        assert!(matches!(
            header.validate(),
            Err(SparseError::UnsupportedVersion(_, _))
        ));
        let mut bad_blk_sz = data.clone();
        bad_blk_sz[12..16].copy_from_slice(&0u32.to_le_bytes());
        let header = SparseHeader::from_bytes(&bad_blk_sz).unwrap();
        assert!(matches!(
            header.validate(),
            Err(SparseError::InvalidHeader(_))
        ));
    }

    #[test]
    fn test_chunk_header_from_bytes() {
        let mut data = vec![0u8; CHUNK_HEADER_SIZE];
        data[0..2].copy_from_slice(&0xCAC1u16.to_le_bytes());
        data[2..4].copy_from_slice(&0u16.to_le_bytes());
        data[4..8].copy_from_slice(&10u32.to_le_bytes());
        data[8..12].copy_from_slice(&40972u32.to_le_bytes());

        let chunk = ChunkHeader::from_bytes(&data).unwrap();
        assert_eq!(chunk.get_type().unwrap(), ChunkType::Raw);
        assert_eq!(chunk.chunk_sz, 10);
        assert_eq!(chunk.data_size(), 40972 - 12);
    }

    #[test]
    fn test_is_sparse_data() {
        let sparse_data = [0x3A, 0xFF, 0x26, 0xED, 0x00, 0x00];
        assert!(is_sparse_data(&sparse_data));
        let not_sparse = [0x00, 0x00, 0x00, 0x00];
        assert!(!is_sparse_data(&not_sparse));
        let too_short = [0x3A, 0xFF];
        assert!(!is_sparse_data(&too_short));
    }

    #[test]
    fn test_sparse_writer_basic() {
        let mut writer = SparseWriter::new(4096, 4096 * 10);
        writer.add_chunk(SparseChunk::Raw(vec![0xAB; 4096]));
        writer.add_chunk(SparseChunk::Fill {
            value: 0,
            blocks: 5,
        });
        writer.add_chunk(SparseChunk::DontCare { blocks: 4 });

        let data = writer.build();
        let header = SparseHeader::from_bytes(&data).unwrap();
        assert!(header.validate().is_ok());
        assert_eq!(header.blk_sz, 4096);
        assert_eq!(header.total_chunks, 3);
        assert_eq!(header.total_blks, 10);
    }

    #[test]
    fn test_sparse_writer_roundtrip() {
        let mut writer = SparseWriter::new(4096, 4096 * 100);
        writer.add_chunk(SparseChunk::Raw(vec![0x11; 4096 * 2]));
        writer.add_chunk(SparseChunk::Fill {
            value: 0xDEADBEEF,
            blocks: 10,
        });
        writer.add_chunk(SparseChunk::DontCare { blocks: 88 });

        let data = writer.build();
        let header = SparseHeader::from_bytes(&data).unwrap();
        assert!(header.validate().is_ok());
        assert_eq!(header.total_blks, 100);
        assert_eq!(header.total_chunks, 3);
    }

    #[test]
    fn test_sparse_chunk_transfer_size() {
        let raw = SparseChunk::Raw(vec![0; 1000]);
        assert_eq!(raw.transfer_size(), CHUNK_HEADER_SIZE as u64 + 1000);
        let fill = SparseChunk::Fill {
            value: 0,
            blocks: 100,
        };
        assert_eq!(fill.transfer_size(), CHUNK_HEADER_SIZE as u64 + 4);
        let dont_care = SparseChunk::DontCare { blocks: 100 };
        assert_eq!(dont_care.transfer_size(), CHUNK_HEADER_SIZE as u64);
        let crc = SparseChunk::Crc32(0x12345678);
        assert_eq!(crc.transfer_size(), CHUNK_HEADER_SIZE as u64 + 4);
    }

    #[test]
    fn test_sparse_chunk_output_blocks() {
        let block_size = 4096u32;
        let raw = SparseChunk::Raw(vec![0; 4096 * 3]);
        assert_eq!(raw.output_blocks(block_size), 3);
        let raw_small = SparseChunk::Raw(vec![0; 100]);
        assert_eq!(raw_small.output_blocks(block_size), 1);
        let fill = SparseChunk::Fill {
            value: 0,
            blocks: 50,
        };
        assert_eq!(fill.output_blocks(block_size), 50);
        let dont_care = SparseChunk::DontCare { blocks: 25 };
        assert_eq!(dont_care.output_blocks(block_size), 25);
        let crc = SparseChunk::Crc32(0);
        assert_eq!(crc.output_blocks(block_size), 0);
    }
    fn create_test_sparse(block_size: u32, chunks: &[SparseChunk]) -> Vec<u8> {
        let total_blocks: u32 = chunks.iter().map(|c| c.output_blocks(block_size)).sum();
        let mut writer = SparseWriter::new(block_size, block_size as u64 * total_blocks as u64);
        for chunk in chunks {
            writer.add_chunk(chunk.clone());
        }
        writer.build()
    }

    #[test]
    fn test_resparse_no_split_needed() {
        let block_size = 4096u32;
        let chunks = vec![
            SparseChunk::Raw(vec![0xAB; 4096]),
            SparseChunk::Fill {
                value: 0,
                blocks: 2,
            },
        ];
        let data = create_test_sparse(block_size, &chunks);

        let header = SparseHeader::from_bytes(&data).unwrap();
        assert!(header.validate().is_ok());
        assert_eq!(header.total_blks, 3);
    }

    #[test]
    fn test_sparse_writer_preserves_expanded_size() {
        let block_size = 4096u32;
        let test_cases = vec![
            vec![SparseChunk::Raw(vec![0; 4096 * 5])],
            vec![SparseChunk::Fill {
                value: 0xDEAD,
                blocks: 10,
            }],
            vec![SparseChunk::DontCare { blocks: 20 }],
            vec![
                SparseChunk::Raw(vec![0x11; 4096 * 2]),
                SparseChunk::Fill {
                    value: 0,
                    blocks: 5,
                },
                SparseChunk::DontCare { blocks: 3 },
            ],
        ];

        for chunks in test_cases {
            let expected_blocks: u32 = chunks.iter().map(|c| c.output_blocks(block_size)).sum();
            let expected_size = block_size as u64 * expected_blocks as u64;

            let data = create_test_sparse(block_size, &chunks);
            let header = SparseHeader::from_bytes(&data).unwrap();

            assert_eq!(
                header.expanded_size(),
                expected_size,
                "展开大小不匹配: expected {}, got {}",
                expected_size,
                header.expanded_size()
            );
        }
    }

    #[test]
    fn test_sparse_transfer_size_calculation() {
        let block_size = 4096u32;
        let chunks = vec![
            SparseChunk::Raw(vec![0xAB; 4096 * 2]),
            SparseChunk::Fill {
                value: 0,
                blocks: 5,
            },
            SparseChunk::DontCare { blocks: 3 },
        ];

        let expected_transfer = SPARSE_HEADER_SIZE as u64
            + (CHUNK_HEADER_SIZE as u64 + 4096 * 2)
            + (CHUNK_HEADER_SIZE as u64 + 4)
            + CHUNK_HEADER_SIZE as u64;

        let data = create_test_sparse(block_size, &chunks);
        assert_eq!(data.len() as u64, expected_transfer);
    }
}

#[cfg(test)]
mod property_tests {
    use super::*;
    use proptest::prelude::*;
    fn arb_sparse_chunk(block_size: u32) -> impl Strategy<Value = SparseChunk> {
        prop_oneof![
            (1u32..=10).prop_map(move |blocks| {
                SparseChunk::Raw(vec![0xAB; (block_size * blocks) as usize])
            }),
            (any::<u32>(), 1u32..=20)
                .prop_map(|(value, blocks)| { SparseChunk::Fill { value, blocks } }),
            (1u32..=50).prop_map(|blocks| { SparseChunk::DontCare { blocks } }),
        ]
    }

    proptest! {


        #[test]
        fn prop_resparse_preserves_expanded_size(
            chunks in prop::collection::vec(arb_sparse_chunk(4096), 1..5)
        ) {
            let block_size = 4096u32;
            let total_blocks: u32 = chunks.iter().map(|c| c.output_blocks(block_size)).sum();

            if total_blocks == 0 {
                return Ok(());
            }

            let mut writer = SparseWriter::new(block_size, block_size as u64 * total_blocks as u64);
            for chunk in &chunks {
                writer.add_chunk(chunk.clone());
            }
            let data = writer.build();
            let header = SparseHeader::from_bytes(&data).unwrap();
            prop_assert!(header.validate().is_ok());
            let expected_expanded = block_size as u64 * total_blocks as u64;
            prop_assert_eq!(header.expanded_size(), expected_expanded);
        }
        #[test]
        fn prop_sparse_writer_produces_valid_header(
            block_size in prop::sample::select(vec![512u32, 1024, 2048, 4096, 8192]),
            num_blocks in 1u32..100
        ) {
            let mut writer = SparseWriter::new(block_size, block_size as u64 * num_blocks as u64);
            writer.add_chunk(SparseChunk::DontCare { blocks: num_blocks });

            let data = writer.build();
            let header = SparseHeader::from_bytes(&data).unwrap();

            prop_assert!(header.validate().is_ok());
            prop_assert_eq!(header.magic, SPARSE_HEADER_MAGIC);
            prop_assert_eq!(header.major_version, 1);
            prop_assert_eq!(header.blk_sz, block_size);
            prop_assert_eq!(header.total_blks, num_blocks);
        }
        #[test]
        fn prop_chunk_transfer_size_correct(
            raw_size in 1usize..10000,
            fill_blocks in 1u32..100,
            dont_care_blocks in 1u32..100
        ) {
            let raw = SparseChunk::Raw(vec![0; raw_size]);
            prop_assert_eq!(raw.transfer_size(), CHUNK_HEADER_SIZE as u64 + raw_size as u64);
            let fill = SparseChunk::Fill { value: 0, blocks: fill_blocks };
            prop_assert_eq!(fill.transfer_size(), CHUNK_HEADER_SIZE as u64 + 4);
            let dont_care = SparseChunk::DontCare { blocks: dont_care_blocks };
            prop_assert_eq!(dont_care.transfer_size(), CHUNK_HEADER_SIZE as u64);
        }
    }
}
