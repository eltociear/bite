//! Rust v0 symbol demangler

use super::TokenStream;
use crate::colors;

const MAX_DEPTH: usize = 256;

pub fn parse(s: &str) -> Option<TokenStream> {
    // paths have to be ascii
    if !s.bytes().all(|c| c.is_ascii()) {
        return None;
    }

    let mut parser = Parser::new(s);
    parser.path();
    dbg!(parser.stream.tokens());

    Some(parser.stream)
}

/// Rust v0 state required to parse symbols
struct Parser {
    stream: TokenStream,
    offset: usize,
    depth: usize,
}

enum NameSpace {
    Closure,
    Shim,
    Special(char),
    Internal(char)
}

impl Parser {
    /// Create an initialized parser that hasn't started parsing yet.
    fn new(s: &str) -> Self {
        Self {
            stream: TokenStream::new(s),
            offset: 0,
            depth: 0,
        }
    }

    /// Create a reference to the underlying pinned string that holds the mangled symbol.
    fn src(&self) -> &'static str {
        &self.stream.inner()[self.offset..]
    }

    /// View the current byte in the mangled symbol without incrementing the offset.
    fn peek(&self) -> Option<u8> {
        self.src().bytes().next()
    }

    fn peek_slice(&self, range: std::ops::Range<usize>) -> Option<&str> {
        self.src().get(range)
    }

    /// Increment the offset if the current byte equals the byte given.
    fn consume(&mut self, byte: u8) -> Option<()> {
        if self.src().bytes().next() == Some(byte) {
            self.offset += 1;
            return Some(());
        }

        None
    }

    /// Consumes a series of bytes that are in the range b'0' to b'Z' and converts it to base 62.
    /// It also consumes an underscore in the case that the base62 number happens to be zero.
    fn base62(&mut self) -> Option<usize> {
        let mut num = 0usize;

        if let Some(..) = self.consume(b'_') {
            return Some(num);
        }

        while let Some(chr) = self.peek() {
            let base_62_chr = match chr {
                b'0'..=b'9' => chr - b'0',
                b'a'..=b'z' => chr - b'a' + 10,
                b'A'..=b'Z' => chr - b'A' + 36,
                b'_' => return num.checked_add(1),
                _ => return None,
            };

            num = num.checked_mul(62)?;
            num = num.checked_add(base_62_chr as usize)?;

            self.offset += 1;
        }

        None
    }

    /// Consumes a series of bytes that are in the range b'0' to b'9' and converts it to base 10.
    fn base10(&mut self) -> Option<usize> {
        let mut len = 0usize;

        while let Some(digit @ b'0'..=b'9') = self.peek() {
            len = len.checked_mul(10)?;
            len = len.checked_add((digit - b'0') as usize)?;

            self.offset += 1;
        }

        Some(len)
    }

    /// Consumes an ident's disambiguator.
    fn disambiguator(&mut self) -> Option<usize> {
        if let Some(..) = self.consume(b's') {
            self.offset += 1;
            return self.base62();
        }

        None
    }

    /// Consumes either a regular unambiguous or a punycode enabled string.
    fn ident<'src>(&mut self) -> Option<&'src str> {
        if let Some(..) = self.consume(b'u') {
            todo!("punycode symbols decoding");
        }

        let len = self.base10()?;
        let _underscore = self.consume(b'_');

        self.src().get(..len).map(|slice| {
            self.offset += slice.len();
            slice
        })
    }

    fn namespace(&mut self) -> Option<NameSpace> {
        let ns = match self.peek()? {
            b'C' => Some(NameSpace::Closure),
            b'S' => Some(NameSpace::Shim),
            c @ b'A'..=b'Z' => Some(NameSpace::Special(c as char)),
            c @ b'a'..=b'z' => Some(NameSpace::Internal(c as char)),
            _ => return None
        };

        self.offset += 1;
        ns
    }

    fn generic(&mut self) -> Option<()> {
        if let Some(..) = self.lifetime() {
            return Some(());
        }

        if let Some(..) = self.tipe() {
            return Some(());
        }

        if let Some(..) = self.consume(b'K') {
            self.constant()?;
            return Some(());
        }

        None
    }

    fn constant(&mut self) -> Option<()> {
        None
    }

    fn lifetime(&mut self) -> Option<()> {
        self.consume(b'L')?;

        if self.base62()? != 0 {
            self.stream.push("'_", colors::MAGENTA);
        }

        Some(())
    }

    fn binder(&mut self) -> Option<usize> {
        self.consume(b'G')?;
        self.base62()
    }

    fn basic_tipe(&mut self, prefix: u8) -> Option<&'static str> {
        let basic = match prefix {
            b'b' => "bool",
            b'c' => "char",
            b'e' => "str",
            b'u' => "()",
            b'a' => "i8",
            b's' => "i16",
            b'l' => "i32",
            b'x' => "i64",
            b'n' => "i128",
            b'i' => "isize",
            b'h' => "u8",
            b't' => "u16",
            b'm' => "u32",
            b'y' => "u64",
            b'o' => "u128",
            b'j' => "usize",
            b'f' => "f32",
            b'd' => "f64",
            b'z' => "!",
            b'p' => "_",
            b'v' => "...",
            _ => return None,
        };

        self.offset += 1;
        Some(basic)
    }

    fn path(&mut self) -> Option<()> {
        if self.depth > MAX_DEPTH {
            return None;
        }

        self.depth += 1;

        match self.peek()? {
            // crate root
            b'C' => {
                self.offset += 1;

                let _disambiguator = self.disambiguator();
                let ident = self.ident()?;
                self.stream.push(ident, colors::PURPLE);
            }
            // <T> (inherited impl)
            b'M' => {
                self.offset += 1;

                let _disambiguator = self.disambiguator();
                self.path()?;
                self.stream.push("<", colors::BLUE);
                self.tipe()?;
                self.stream.push(">", colors::BLUE);
            }
            // <T as Trait> (trait impl)
            b'X' => {
                self.offset += 1;

                let _disambiguator = self.disambiguator();
                self.path()?;
                self.tipe()?;
                self.stream.push(" as ", colors::BLUE);
                self.path()?;
            }
            // <T as Trait> (trait definition)
            b'Y' => {
                self.offset += 1;

                self.stream.push("<", colors::BLUE);
                self.tipe()?;
                self.stream.push(" as ", colors::BLUE);
                self.path()?;
                self.stream.push(">", colors::BLUE);
            }
            // ...::ident (nested path)
            b'N' => {
                self.offset += 1;

                let ns = self.namespace()?;
                let _disambiguator = self.disambiguator();
                self.path()?;
                let ident = self.ident()?;

                self.stream.push("::", colors::GRAY);

                match ns {
                    NameSpace::Closure => {
                        self.stream.push("{closure", colors::GRAY);

                        if !ident.is_empty() {
                            self.stream.push(":", colors::GRAY);
                            self.stream.push(ident, colors::GRAY);
                        }

                        self.stream.push("}", colors::GRAY);

                    }
                    _ => self.stream.push(ident, colors::PURPLE),
                }
            }
            // ...<T, U, ..> (generic args)
            b'I' => {
                self.offset += 1;

                let next_is_type = self.peek_slice(0..2) == Some("Nt");

                self.path()?;

                // generics on types shouldn't print a '::'
                if !next_is_type {
                    self.stream.push("::", colors::GRAY);
                }

                self.stream.push("<", colors::BLUE);

                let mut iters = 0;
                while let None = self.consume(b'E') {
                    if iters != 0 {
                        self.stream.push(", ", colors::BLUE);
                    }

                    self.generic()?;
                    iters += 1;
                }

                self.stream.push(">", colors::BLUE);
            }
            _ => return None,
        }

        Some(())
    }

    fn tipe(&mut self) -> Option<()> {
        if self.depth > MAX_DEPTH {
            return None;
        }

        self.depth += 1;

        let prefix = self.peek()?;

        // basic types
        if let Some(tipe) = self.basic_tipe(prefix) {
            self.stream.push(tipe, colors::PURPLE);
            return Some(());
        }

        // named type
        if let Some(..) = self.path() {
            return Some(());
        }

        match prefix {
            // [T; N]
            b'A' => {
                self.stream.push("[", colors::BLUE);
                self.tipe()?;
                self.stream.push("; ", colors::BLUE);
                self.constant()?;
                self.stream.push("]", colors::BLUE);
            }
            // [T]
            b'S' => {
                self.stream.push("[", colors::BLUE);
                self.tipe()?;
                self.stream.push("]", colors::BLUE);
            }
            // (T1, T2, T3, ..)
            b'T' => {
                self.stream.push("(", colors::BLUE);

                let mut iters = 0;
                while let None = self.consume(b'E') {
                    if iters != 0 {
                        self.stream.push(", ", colors::BLUE);
                    }

                    self.tipe();
                    iters += 1;
                }

                self.stream.push(")", colors::BLUE);
            }
            // &T
            b'R' => {
                self.stream.push("&", colors::BLUE);
                self.lifetime()?;
                self.tipe()?;
            }
            // &mut T
            b'Q' => {
                self.stream.push("&mut ", colors::BLUE);
                self.lifetime()?;
                self.tipe()?;
            }
            // *const T
            b'P' => {
                self.stream.push("*const ", colors::BLUE);
                self.tipe()?;
            }
            // *mut T
            b'O' => {
                self.stream.push("*mut ", colors::BLUE);
                self.tipe()?;
            }
            // fn(..) -> ..
            b'F' => {
                self.binder();

                if let Some(..) = self.consume(b'U') {
                    self.stream.push("unsafe ", colors::RED);
                }

                if let Some(..) = self.consume(b'K') {
                    self.stream.push("extern ", colors::RED);

                    if let Some(..) = self.consume(b'C') {
                        self.stream.push("\"C\"", colors::BLUE);
                    } else {
                        let ident = self.ident()?;

                        self.stream.push("\"", colors::BLUE);
                        self.stream.push(ident, colors::BLUE);
                        self.stream.push("\"", colors::BLUE);
                    }
                }

                self.stream.push("fn", colors::MAGENTA);
                self.stream.push("(", colors::WHITE);

                let mut iters = 0;
                while let None = self.consume(b'E') {
                    if iters != 0 {
                        self.stream.push(", ", colors::BLUE);
                    }

                    self.tipe();
                    iters += 1;
                }

                self.stream.push(" -> ", colors::BLUE);
                self.tipe()?;
            }
            // dyn ..
            b'D' => {
                self.binder();
                self.stream.push("dyn ", colors::RED);

                // associated traits e.g. Send + Sync + Pin
                let mut iters = 0;
                while let None = self.consume(b'E') {
                    if iters != 0 {
                        self.stream.push(" + ", colors::BLUE);
                    }

                    self.path()?;

                    // associated trait bounds e.g. Trait<Assoc = X>
                    while let Some(..) = self.consume(b'p') {
                        self.stream.push("<", colors::BLUE);
                        let ident = self.ident()?;
                        self.stream.push(ident, colors::PURPLE);
                        self.stream.push(" = ", colors::WHITE);
                        self.tipe()?;
                        self.stream.push(">", colors::BLUE);
                    }

                    iters += 1;
                }
            }
            b'B' => {
                let backref = self.base62()?;

                todo!("handle backref: {backref}")
            }
            _ => return None,
        }

        Some(())
    }
}

#[cfg(test)]
mod tests {
    macro_rules! eq {
        ($mangled:literal => $demangled:literal) => {
            let symbol = $crate::symbols::rust_modern::parse($mangled)
                .expect(&format!("Formatting '{}' failed.", $mangled));

            assert_eq!(
                String::from_iter(symbol.tokens().iter().map(|t| t.text)),
                $demangled
            )
        };
    }

    #[test]
    fn crate_ident() {
        eq!("C8demangle" => "demangle");
    }

    #[test]
    fn generics() {
        eq!("INvNvC3std3mem8align_ofjdE" => "std::mem::align_of::<usize, f64>");
        eq!("INvNtC3std3mem8align_ofINtC3wow6HolderpEE" =>
             "std::mem::align_of::<wow::Holder<_>>");
    }

    #[test]
    fn namespaces() {
        eq!("NvC4bite6decode" => "bite::decode");
        eq!("NvNvC4bite6decode6x86_64" => "bite::decode::x86_64");
        eq!("INvNvC4bite6decode6x86_64NvC3lol4damnE" =>
             "bite::decode::x86_64::<lol::damn>");
    }

    #[test]
    fn methods() {
        eq!("NvNvXs2_C7mycrateINtC7mycrate3FoopEINtNtC3std7convert4FrompE4from3MSG" =>
             "<mycrate::Foo<_> as std::convert::From<_>>::from::MSG");
    }

    #[test]
    fn pointers() {
        eq!("INvC4bite6decodeRL_eE" => "bite::decode::<&str>");
        eq!("INvC4bite6decodeRL0_eE" => "bite::decode::<&'a str>");

        eq!("INvC4bite6decodeQL_eE" => "bite::decode::<&mut str>");
        eq!("INvC4bite6decodeQL0_eE" => "bite::decode::<&'a mut str>");

        eq!("INvC4bite6decodePeE" => "bite::decode::<*const str>");
        eq!("INvC4bite6decodeOeE" => "bite::decode::<*mut str>");
    }

    #[test]
    fn arrays() {
        eq!("INvC4bite6decodeANtNvC3std5array5Arrayjf_E" =>
             "bite::decode::<[std::array::Array; 15]>");
    }

    #[test]
    fn tupples() {
        eq!("INvNtC3std3mem8align_ofjTddNvC4core3ptrEE" =>
             "std::mem::align_of::<usize, (f64, f64, core::ptr)>");
    }

    #[test]
    fn constants() {
        eq!("NvXs5_NtCsd4VYFwevHkG_4bite6decodeINtB5_5ArrayNtNtB5_6x86_646PrefixKj4_EINtNtNtCs9ltgdHTiPiY_4core3ops5index8IndexMutjE9index_mutB7_" =>
             "<bite::decode::Array<bite::decode::x86_64::Prefix, 4> as core::ops::index::IndexMut<usize>>::index_mut");

        eq!("_NvMNtCs9ltgdHTiPiY_4core5sliceSRe4iterCslWKjbRFJPpS_3log" => "<[&str]>::iter");

        eq!("_NvMs1_NtNtCs9ltgdHTiPiY_4core3ptr8non_nullINtB5_7NonNullReE6as_ptrCslWKjbRFJPpS_3log" =>
             "<core::ptr::non_null::NonNull<&str>>::as_ptr");
    }

    #[test]
    fn fn_signature() {
        eq!("INvNtC3std3mem8align_ofFUKC3rundddEoE" =>
             "std::mem::align_of::<unsafe fn run(f64, f64, f64) -> u128>");

        eq!("INvNtC3std3mem8align_ofFKC3rundddEoE" =>
             "std::mem::align_of::<fn run(f64, f64, f64) -> u128>");

        eq!("INvNtC3std3mem8align_ofFdddEoE" =>
             "std::mem::align_of::<fn(f64, f64, f64) -> u128>");
    }

    #[test]
    fn dyn_traits() {
        eq!("INvNtC4core4simd3mulDNvNtC4core3mem4Readp4ItemReEL_E" =>
             "core::simd::mul::<dyn core::mem::Read<Item = &str>>");

        eq!("INvNtC4core4simd3mulDNvNtC4core3mem4ReadEL0_E" =>
             "core::simd::mul::<dyn core::mem::Read + 'a>");

        eq!("INvNtC4core4simd3mulDNvNtC4core3mem4ReadEL_E" =>
             "core::simd::mul::<dyn core::mem::Read>");
    }

    #[test]
    fn type_compression() {
        eq!("INvNtCs9ltgdHTiPiY_4core3ptr13drop_in_placeNtCs1GtwyVVVJ4z_6goblin6ObjectECsjO9TEQ1PNLx_4bite" =>
             "core::ptr::drop_in_place::<goblin::Object>");
    }

    #[test]
    fn closures() {
        eq!("NCNvC4bite6decodes_0" => "bite::decode::{closure}");
        eq!("NCNvC4bite6decodes0_" => "bite::decode::{closure#1}");
        eq!("NCNvC4bite6decodes0_3wow" => "bite::decode::{closure:wow#1}");

        eq!("INvMNtCs9ltgdHTiPiY_4core6optionINtB3_6OptionRhE3maphNCINvMs9_NtCsd4VYFwevHkG_4bite6decodeNtBZ_6Reader10consume_eqNCNvNtBZ_6x86_643asms_0Es0_0EB11_" =>
             "<core::option::Option<&u8>>::map::<u8, <bite::decode::Reader>::consume_eq::<bite::decode::x86_64::asm::{closure}>::{closure#1}>");
    }
}
