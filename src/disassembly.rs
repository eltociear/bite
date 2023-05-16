use crate::symbols::Index;

use decoder::encode_hex_bytes_truncated;
use decoder::{Decodable, Decoded, Failed};
use object::{Object, ObjectSection, SectionKind};

use std::collections::BTreeMap;
use std::collections::VecDeque;
use std::ops::Bound;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

#[derive(Debug)]
pub enum DecodeError {
    /// Unexpected read of binary failed.
    ReadFailed(std::io::Error),

    /// Failed to find a section with the given entrypoint.
    NoEntrypoint,

    /// Failed to decompress a given section section.
    DecompressionFailed(object::Error),

    /// Failed to parse object.
    IncompleteObject(object::Error),

    /// Failed to parse import table.
    IncompleteImportTable(object::Error),

    /// Failed to parse symbols table.
    IncompleteSymbolTable(pdb::Error),

    /// Decoder support for this platform doesn't yet exist.
    UnknownArchitecture,
}

pub struct Disassembly {
    /// Where the cursor is currently.
    pub current_addr: usize,

    /// Processor which holds information related to each instruction.
    pub proc: Box<dyn InspectProcessor + Send>,

    /// Symbol lookup by absolute address.
    pub symbols: Index,
}

impl Disassembly {
    pub fn new<P: AsRef<std::path::Path>>(
        path: P,
        show_donut: Arc<AtomicBool>,
    ) -> Result<Self, DecodeError> {
        let now = tokio::time::Instant::now();
        show_donut.store(true, Ordering::Relaxed);

        let binary = std::fs::read(&path).map_err(DecodeError::ReadFailed)?;
        let obj = object::File::parse(&binary[..]).map_err(DecodeError::IncompleteObject)?;

        let entrypoint = obj.entry();
        let section = obj
            .sections()
            .filter(|s| s.kind() == SectionKind::Text)
            .find(|t| (t.address()..t.address() + t.size()).contains(&entrypoint))
            .ok_or(DecodeError::NoEntrypoint)?;

        let raw = section
            .uncompressed_data()
            .map_err(DecodeError::DecompressionFailed)?
            .into_owned();

        let section_base = section.address() as usize;
        let mut symbols = Index::new();

        symbols.parse_debug(&obj).map_err(DecodeError::IncompleteSymbolTable)?;

        if obj.format() == object::BinaryFormat::Pe {
            if obj.is_64() {
                symbols
                    .parse_imports::<object::pe::ImageNtHeaders64>(&binary[..])
                    .map_err(DecodeError::IncompleteImportTable)?;
            } else {
                symbols
                    .parse_imports::<object::pe::ImageNtHeaders32>(&binary[..])
                    .map_err(DecodeError::IncompleteImportTable)?;
            }
        }

        let proc: Box<dyn InspectProcessor + Send> = match obj.architecture() {
            object::Architecture::Riscv32 => {
                let decoder = disassembler::riscv::Decoder { is_64: false };

                let mut proc: Processor<disassembler::riscv::Decoder> =
                    Processor::new(raw, section_base, obj.entry() as usize, decoder);

                proc.recurse(&symbols);
                Box::new(proc)
            }
            object::Architecture::Riscv64 => {
                let decoder = disassembler::riscv::Decoder { is_64: true };

                let mut proc: Processor<disassembler::riscv::Decoder> =
                    Processor::new(raw, section_base, obj.entry() as usize, decoder);

                proc.recurse(&symbols);
                Box::new(proc)
            }
            object::Architecture::Mips | object::Architecture::Mips64 => {
                let decoder = disassembler::mips::Decoder::default();

                let mut proc: Processor<disassembler::mips::Decoder> =
                    Processor::new(raw, section_base, obj.entry() as usize, decoder);

                proc.recurse(&symbols);
                Box::new(proc)
            }
            object::Architecture::X86_64_X32 => {
                let decoder = disassembler::x86::Decoder::default();

                let mut proc: Processor<disassembler::x86::Decoder> =
                    Processor::new(raw, section_base, obj.entry() as usize, decoder);

                proc.recurse(&symbols);
                Box::new(proc)
            }
            object::Architecture::X86_64 => {
                let decoder = disassembler::x64::Decoder::default();

                let mut proc: Processor<disassembler::x64::Decoder> =
                    Processor::new(raw, section_base, obj.entry() as usize, decoder);

                proc.recurse(&symbols);
                Box::new(proc)
            }
            _ => return Err(DecodeError::UnknownArchitecture),
        };

        println!("took {:#?} to parse {:?}", now.elapsed(), path.as_ref());
        Ok(Self {
            current_addr: 0,
            proc,
            symbols,
        })
    }
}

#[derive(Debug)]
pub struct Metadata<D: Decoded> {
    instruction: D,
}

impl<D: Decoded> Metadata<D> {
    fn new(
        addr: usize,
        symbols: &Index,
        mut instruction: D,
    ) -> Self {
        instruction.find_xrefs(addr, &symbols.tree);
        Self {
            instruction,
        }
    }
}

/// Recursive decent disassembler that inspect one given section.
/// It currently has the limitation of only being able to inspect the section
/// where a given binaries entrypoint is.
#[derive(Debug)]
pub struct Processor<D: decoder::Decodable> {
    pub section: Vec<u8>,
    pub entrypoint: usize,
    pub base_addr: usize,
    pub decoder: D,
    pub parsed: BTreeMap<usize, Result<Metadata<D::Instruction>, D::Error>>,
}

impl<D: Decodable> Processor<D> {
    pub fn new(section: Vec<u8>, base_addr: usize, entrypoint: usize, decoder: D) -> Self {
        Self {
            section,
            entrypoint,
            base_addr,
            decoder,
            parsed: BTreeMap::new(),
        }
    }

    pub fn recurse(&mut self, symbols: &Index) {
        let mut unexplored_data = VecDeque::with_capacity(1024);
        let mut raw_instructions = VecDeque::with_capacity(1024);

        // TODO: recurse starting from entrypoint, following jumps
        // unexplored_data.push_back(self.entrypoint);
        unexplored_data.push_back(self.base_addr);

        match self.entrypoint.checked_sub(self.base_addr) {
            Some(entrypoint) => unexplored_data.push_back(entrypoint),
            None => {
                eprintln!("failed to calculate entrypoint, defaulting to 0x1000");
                unexplored_data.push_back(self.base_addr + 0x1000);
            }
        }

        while let Some(addr) = unexplored_data.pop_front() {
            // don't visit addresses that are already decoded
            if self.parsed.contains_key(&addr) {
                continue;
            }

            // don't visit addresses that are outside of the section
            let bytes = match self.bytes_by_addr(addr) {
                Some(bytes) => bytes,
                None => continue,
            };

            let mut reader = decoder::Reader::new(bytes);
            let instruction = self.decoder.decode(&mut reader);
            let width = match instruction {
                Ok(inst) => {
                    let width = inst.width();
                    raw_instructions.push_back((addr, inst));
                    width
                }
                Err(err) if !err.is_complete() => continue,
                Err(err) => {
                    let width = err.incomplete_width();
                    self.parsed.insert(addr, Err(err));
                    width
                }
            };

            unexplored_data.push_back(addr + width);
        }

        while let Some((addr, instruction)) = raw_instructions.pop_front() {
            let meta = Metadata::new(addr, symbols, instruction);

            self.parsed.insert(addr, Ok(meta));
        }
    }

    fn bytes_by_addr<'a>(&'a self, addr: usize) -> Option<&'a [u8]> {
        addr.checked_sub(self.base_addr).and_then(|addr| self.section.get(addr..))
    }
}

pub type MaybeInstruction<'a> = Result<&'a dyn Decoded, &'a dyn Failed>;

pub trait InspectProcessor {
    fn iter(&self) -> Box<dyn DoubleEndedIterator<Item = (usize, MaybeInstruction)> + '_>;
    fn in_range(
        &self,
        start: Bound<usize>,
        end: Bound<usize>,
    ) -> Box<dyn DoubleEndedIterator<Item = (usize, MaybeInstruction)> + '_>;

    fn instruction_count(&self) -> usize;
    fn base_addr(&self) -> usize;
    fn section(&self) -> &[u8];
    fn bytes(&self, instruction: MaybeInstruction, addr: usize) -> String;
}

impl<D: Decodable> InspectProcessor for Processor<D> {
    fn iter(&self) -> Box<dyn DoubleEndedIterator<Item = (usize, MaybeInstruction)> + '_> {
        Box::new(self.parsed.iter().map(|(addr, inst)| {
            (
                *addr,
                match inst {
                    Ok(ref val) => Ok(&val.instruction as &dyn Decoded),
                    Err(ref err) => Err(err as &dyn Failed),
                },
            )
        }))
    }

    fn in_range(
        &self,
        start: Bound<usize>,
        end: Bound<usize>,
    ) -> Box<dyn DoubleEndedIterator<Item = (usize, MaybeInstruction)> + '_> {
        Box::new(self.parsed.range((start, end)).map(|(addr, inst)| {
            (
                *addr,
                match inst {
                    Ok(ref val) => Ok(&val.instruction as &dyn Decoded),
                    Err(ref err) => Err(err as &dyn Failed),
                },
            )
        }))
    }

    fn instruction_count(&self) -> usize {
        self.parsed.len()
    }

    fn base_addr(&self) -> usize {
        self.base_addr
    }

    fn section(&self) -> &[u8] {
        &self.section[..]
    }

    fn bytes(&self, instruction: MaybeInstruction, addr: usize) -> String {
        let rva = addr - self.base_addr;
        let bytes = match instruction {
            Ok(instruction) => &self.section[rva..][..instruction.width()],
            Err(err) => &self.section[rva..][..err.incomplete_width()],
        };

        encode_hex_bytes_truncated(bytes, self.decoder.max_width() * 3 + 1)
    }
}
