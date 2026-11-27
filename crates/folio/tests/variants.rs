//! What one variant hides never reaches another variant's PDF (SPEC §6.4):
//! not its text, and not the alternate descriptions of its formulas, which
//! are read from the evaluated math tree rather than the source (#318).

use folio::{Name, Target, World};

const MANIFEST: &str = "[project]\nentry = \"main.fol\"\nedition = \"2027\"\n\
                        compiler = \"0.0.0\"\nlang = \"en\"\n\n\
                        [variants.exam]\ntargets = [\"pdf\"]\n\n\
                        [variants.markscheme]\ntargets = [\"pdf\"]\n";

/// A paper whose mark scheme hides answers in formulas, in comments inside
/// formulas, and in content of its own.
const MAIN: &str = r#"#doc(title: [Forces])
#let ans(body) = if variant == "markscheme" { body } else { [] }

= Question 1

Solve $x + 1 = 43 /* ANSWERCOMMENT */$ for $x$.

The answer is $x = #ans($"SECRETFORTYTWO"$)$.

$ y = 2 x #if variant == "markscheme" { $= "HIDDENEIGHTYFOUR"$ } $

#if variant == "markscheme" [
  = HIDDENHEADING

  HIDDENPARAGRAPH $z = "HIDDENFORMULA"$.
]
"#;

/// The strings the exam must not hold.
const HIDDEN: [&str; 5] = [
    "ANSWERCOMMENT",
    "SECRETFORTYTWO",
    "HIDDENEIGHTYFOUR",
    "HIDDENHEADING",
    "HIDDENFORMULA",
];

fn pdf(world: &World, variant: &str) -> Vec<u8> {
    let variant = Name::new(variant).unwrap();
    let compiled = folio::compile(world, Target::Pdf, Some(&variant)).unwrap();
    compiled.output.clone().expect("a PDF").to_vec()
}

#[test]
fn hidden_content_and_formula_source_do_not_reach_the_exam_pdf() {
    let mut world = World::new(MANIFEST).unwrap();
    world.set_source("main.fol", MAIN).unwrap();
    let exam = searchable(&pdf(&world, "exam"));
    for hidden in HIDDEN {
        assert!(!holds(&exam, hidden), "the exam PDF holds {hidden}");
    }
    // The formulas are still described, from what evaluation made of them.
    assert!(holds(&exam, "x + 1 = 43"), "the first formula's reading");
    assert!(holds(&exam, "y = 2 x"), "the display's reading");
    // The search finds what the mark scheme shows, compressed or not.
    let markscheme = searchable(&pdf(&world, "markscheme"));
    for shown in ["SECRETFORTYTWO", "HIDDENEIGHTYFOUR", "HIDDENHEADING"] {
        assert!(holds(&markscheme, shown), "the mark scheme holds {shown}");
    }
    assert!(!holds(&markscheme, "ANSWERCOMMENT"), "comments never do");
}

/// Each instance of a formula that a loop makes reads as evaluation made
/// it, not as the first instance does.
#[test]
fn each_instance_of_a_formula_has_its_own_alt_text() {
    let mut world = World::new(MANIFEST).unwrap();
    world
        .set_source(
            "main.fol",
            "#for v in (17, 29) [Value $x = #v$.\n\n$ y = #v $\n\n]\n",
        )
        .unwrap();
    let pdf = searchable(&pdf(&world, "exam"));
    for alt in ["x = 17", "x = 29", "y = 17", "y = 29"] {
        assert!(holds(&pdf, alt), "{alt}");
    }
}

/// A PDF's bytes followed by every Flate stream of it, inflated.
fn searchable(pdf: &[u8]) -> Vec<u8> {
    let mut out = pdf.to_vec();
    let mut at = 0;
    while let Some(start) = find(&pdf[at..], b"stream").map(|i| at + i) {
        at = start + b"stream".len();
        // `endstream` also ends in `stream`.
        if pdf[..start].ends_with(b"end") {
            continue;
        }
        let mut body = at;
        if pdf.get(body) == Some(&b'\r') {
            body += 1;
        }
        if pdf.get(body) == Some(&b'\n') {
            body += 1;
        }
        let Some(end) = find(&pdf[body..], b"endstream").map(|i| body + i) else {
            break;
        };
        // The stream's object, from `obj` to `stream`, holds its dictionary.
        let object = pdf[..start]
            .windows(3)
            .rposition(|w| w == b"obj")
            .unwrap_or(0);
        let dict = &pdf[object..start];
        if find(dict, b"FlateDecode").is_some() {
            // A zlib stream: two bytes of header before the deflate data.
            let inflated = inflate(&pdf[body + 2..end])
                .unwrap_or_else(|| panic!("an inflatable stream at byte {body}"));
            out.extend_from_slice(b"\n");
            out.extend(inflated);
        }
        at = end;
    }
    out
}

/// Whether bytes hold a string as PDF can write it: as bytes, as UTF-16BE,
/// or either of those in hexadecimal.
fn holds(bytes: &[u8], text: &str) -> bool {
    let utf16: Vec<u8> = text.encode_utf16().flat_map(u16::to_be_bytes).collect();
    let hex = |raw: &[u8], upper: bool| -> Vec<u8> {
        raw.iter()
            .flat_map(|b| {
                let s = if upper {
                    format!("{b:02X}")
                } else {
                    format!("{b:02x}")
                };
                s.into_bytes()
            })
            .collect()
    };
    [
        text.as_bytes().to_vec(),
        utf16.clone(),
        hex(text.as_bytes(), true),
        hex(text.as_bytes(), false),
        hex(&utf16, true),
        hex(&utf16, false),
    ]
    .iter()
    .any(|needle| find(bytes, needle).is_some())
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Inflates raw deflate data (RFC 1951), or `None` if it is malformed.
fn inflate(data: &[u8]) -> Option<Vec<u8>> {
    let mut bits = Bits {
        data,
        pos: 0,
        bit: 0,
    };
    let mut out = Vec::new();
    loop {
        let last = bits.read(1)? == 1;
        match bits.read(2)? {
            0 => {
                bits.align();
                let len = bits.read(16)? as usize;
                bits.read(16)?;
                let start = bits.pos;
                out.extend_from_slice(data.get(start..start + len)?);
                bits.pos += len;
            }
            1 => {
                let mut lengths = [0u8; 288 + 32];
                lengths[..144].fill(8);
                lengths[144..256].fill(9);
                lengths[256..280].fill(7);
                lengths[280..288].fill(8);
                lengths[288..].fill(5);
                let lit = Huffman::new(&lengths[..288])?;
                let dist = Huffman::new(&lengths[288..])?;
                block(&mut bits, &lit, &dist, &mut out)?;
            }
            2 => {
                let hlit = bits.read(5)? as usize + 257;
                let hdist = bits.read(5)? as usize + 1;
                let hclen = bits.read(4)? as usize + 4;
                const ORDER: [usize; 19] = [
                    16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
                ];
                let mut code_lengths = [0u8; 19];
                for &i in &ORDER[..hclen] {
                    code_lengths[i] = bits.read(3)? as u8;
                }
                let codes = Huffman::new(&code_lengths)?;
                let mut lengths = Vec::with_capacity(hlit + hdist);
                while lengths.len() < hlit + hdist {
                    match codes.decode(&mut bits)? {
                        sym @ 0..=15 => lengths.push(sym as u8),
                        16 => {
                            let prev = *lengths.last()?;
                            let n = 3 + bits.read(2)?;
                            lengths.extend(std::iter::repeat_n(prev, n as usize));
                        }
                        17 => {
                            let n = 3 + bits.read(3)?;
                            lengths.extend(std::iter::repeat_n(0, n as usize));
                        }
                        18 => {
                            let n = 11 + bits.read(7)?;
                            lengths.extend(std::iter::repeat_n(0, n as usize));
                        }
                        _ => return None,
                    }
                }
                let lit = Huffman::new(&lengths[..hlit])?;
                let dist = Huffman::new(&lengths[hlit..hlit + hdist])?;
                block(&mut bits, &lit, &dist, &mut out)?;
            }
            _ => return None,
        }
        if last {
            return Some(out);
        }
    }
}

/// Decodes the symbols of a compressed block up to its end.
fn block(bits: &mut Bits<'_>, lit: &Huffman, dist: &Huffman, out: &mut Vec<u8>) -> Option<()> {
    const LEN_BASE: [u16; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
        131, 163, 195, 227, 258,
    ];
    const LEN_EXTRA: [u8; 29] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];
    const DIST_BASE: [u16; 30] = [
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
        2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
    ];
    const DIST_EXTRA: [u8; 30] = [
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
        13, 13,
    ];
    loop {
        let sym = lit.decode(bits)? as usize;
        match sym {
            0..=255 => out.push(sym as u8),
            256 => return Some(()),
            _ => {
                let i = sym - 257;
                let len = *LEN_BASE.get(i)? as usize + bits.read(*LEN_EXTRA.get(i)?)? as usize;
                let d = dist.decode(bits)? as usize;
                let back = *DIST_BASE.get(d)? as usize + bits.read(*DIST_EXTRA.get(d)?)? as usize;
                let from = out.len().checked_sub(back)?;
                for k in 0..len {
                    out.push(out[from + k]);
                }
            }
        }
    }
}

/// Reads bits, least significant first.
struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    bit: u8,
}

impl Bits<'_> {
    fn read(&mut self, n: u8) -> Option<u32> {
        let mut value = 0;
        for i in 0..n {
            let byte = *self.data.get(self.pos)?;
            value |= u32::from((byte >> self.bit) & 1) << i;
            self.bit += 1;
            if self.bit == 8 {
                self.bit = 0;
                self.pos += 1;
            }
        }
        Some(value)
    }

    fn align(&mut self) {
        if self.bit > 0 {
            self.bit = 0;
            self.pos += 1;
        }
    }
}

/// A canonical Huffman code, by the number of codes of each length and the
/// symbols in code order.
struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Option<Self> {
        let mut counts = [0u16; 16];
        for &len in lengths {
            *counts.get_mut(len as usize)? += 1;
        }
        counts[0] = 0;
        let mut offsets = [0u16; 16];
        for len in 1..16 {
            offsets[len] = offsets[len - 1] + counts[len - 1];
        }
        let mut symbols = vec![0; lengths.len()];
        for (sym, &len) in lengths.iter().enumerate() {
            if len > 0 {
                symbols[offsets[len as usize] as usize] = sym as u16;
                offsets[len as usize] += 1;
            }
        }
        Some(Self { counts, symbols })
    }

    fn decode(&self, bits: &mut Bits<'_>) -> Option<u16> {
        let (mut code, mut first, mut index) = (0i32, 0i32, 0i32);
        for len in 1..16 {
            code |= bits.read(1)? as i32;
            let count = i32::from(self.counts[len]);
            if code - first < count {
                return self.symbols.get((index + code - first) as usize).copied();
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        None
    }
}
