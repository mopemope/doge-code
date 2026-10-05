//! Fixed-size, strict UTF-8 traversal with bounded output collectors.
use std::io::{self, Read};

const CHUNK_BYTES: usize = 8 * 1024;

trait Visitor {
    fn raw_char(&mut self, _ch: char) {}
    fn line_char(&mut self, _ch: char) {}
    fn end_line(&mut self, _index: usize) {}
}

fn scan(reader: impl Read, visitor: &mut impl Visitor) -> io::Result<usize> {
    scan_chunks(reader, visitor, CHUNK_BYTES)
}

fn scan_chunks(
    mut reader: impl Read,
    visitor: &mut impl Visitor,
    chunk: usize,
) -> io::Result<usize> {
    assert!((1..=CHUNK_BYTES).contains(&chunk));
    let mut bytes = [0u8; CHUNK_BYTES + 3];
    let mut carry = 0;
    let mut pending_cr = false;
    let mut unfinished_line = false;
    let mut lines = 0usize;
    loop {
        let read = match reader.read(&mut bytes[carry..carry + chunk]) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if read == 0 {
            if carry != 0 {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "incomplete UTF-8 at EOF",
                ));
            }
            break;
        }
        let len = carry + read;
        let (valid, remaining) = match std::str::from_utf8(&bytes[..len]) {
            Ok(text) => (text, 0),
            Err(error) if error.error_len().is_none() => {
                let valid = error.valid_up_to();
                (
                    std::str::from_utf8(&bytes[..valid])
                        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
                    len - valid,
                )
            }
            Err(error) => return Err(io::Error::new(io::ErrorKind::InvalidData, error)),
        };
        for ch in valid.chars() {
            visitor.raw_char(ch);
            if ch == '\n' {
                pending_cr = false;
                lines = lines.saturating_add(1);
                visitor.end_line(lines);
                unfinished_line = false;
            } else {
                if pending_cr {
                    visitor.line_char('\r');
                }
                pending_cr = ch == '\r';
                if !pending_cr {
                    visitor.line_char(ch);
                }
                unfinished_line = true;
            }
        }
        bytes.copy_within(len - remaining..len, 0);
        carry = remaining;
    }
    if pending_cr {
        visitor.line_char('\r');
    }
    if unfinished_line {
        lines = lines.saturating_add(1);
        visitor.end_line(lines);
    }
    Ok(lines)
}

#[derive(Debug)]
pub(super) struct Page {
    pub content: String,
    /// Byte offsets after each selected complete line, including empty lines.
    pub line_ends: Vec<usize>,
    pub total_lines: usize,
    pub oversized_first_line: Option<usize>,
}

struct PageCollector {
    page: Page,
    start: usize,
    limit: usize,
    budget: usize,
    index: usize,
    chars: usize,
    line: String,
    line_chars: usize,
    stopped: bool,
}
impl PageCollector {
    fn selected(&self) -> bool {
        !self.stopped && self.index >= self.start && self.page.line_ends.len() < self.limit
    }
    fn remaining(&self) -> usize {
        self.budget
            .saturating_sub(self.chars)
            .saturating_sub(usize::from(!self.page.line_ends.is_empty()))
    }
}
impl Visitor for PageCollector {
    fn line_char(&mut self, ch: char) {
        if self.selected() {
            self.line_chars = self.line_chars.saturating_add(1);
            if self.line_chars <= self.remaining() {
                self.line.push(ch);
            }
        }
    }
    fn end_line(&mut self, index: usize) {
        if self.selected() {
            if self
                .line_chars
                .saturating_add(usize::from(!self.page.line_ends.is_empty()))
                > self.budget.saturating_sub(self.chars)
            {
                if self.page.line_ends.is_empty() {
                    self.page.oversized_first_line = Some(self.line_chars);
                }
                self.stopped = true;
            } else {
                if !self.page.line_ends.is_empty() {
                    self.page.content.push('\n');
                    self.chars += 1;
                }
                self.page.content.push_str(&self.line);
                self.chars += self.line_chars;
                self.page.line_ends.push(self.page.content.len());
            }
        }
        self.line.clear();
        self.line_chars = 0;
        self.index = index;
    }
}

pub(super) fn read_page(
    reader: impl Read,
    start: usize,
    limit: usize,
    budget: usize,
) -> io::Result<Page> {
    let mut collector = PageCollector {
        page: Page {
            content: String::new(),
            line_ends: Vec::new(),
            total_lines: 0,
            oversized_first_line: None,
        },
        start,
        limit,
        budget,
        index: 0,
        chars: 0,
        line: String::new(),
        line_chars: 0,
        stopped: false,
    };
    collector.page.total_lines = scan(reader, &mut collector)?;
    Ok(collector.page)
}

#[derive(Debug)]
pub(super) struct Snippet {
    pub text: String,
    pub chars: usize,
    pub total_lines: usize,
}
struct SnippetCollector {
    snippet: Snippet,
    line_limit: Option<usize>,
    cap: usize,
    index: usize,
    started: bool,
}
impl SnippetCollector {
    fn push(&mut self, ch: char) {
        if self.snippet.chars < self.cap {
            self.snippet.text.push(ch);
        }
        self.snippet.chars = self.snippet.chars.saturating_add(1);
    }
    fn begin_line(&mut self) {
        if !self.started {
            if self.index != 0 {
                self.push('\n');
            }
            self.started = true;
        }
    }
}
impl Visitor for SnippetCollector {
    fn raw_char(&mut self, ch: char) {
        if self.line_limit.is_none() {
            self.push(ch);
        }
    }
    fn line_char(&mut self, ch: char) {
        if self.line_limit.is_some_and(|limit| self.index < limit) {
            self.begin_line();
            self.push(ch);
        }
    }
    fn end_line(&mut self, index: usize) {
        if self.line_limit.is_some_and(|limit| self.index < limit) {
            self.begin_line();
        }
        self.index = index;
        self.started = false;
    }
}

pub(super) fn read_snippet(
    reader: impl Read,
    line_limit: Option<usize>,
    cap: usize,
) -> io::Result<Snippet> {
    let mut collector = SnippetCollector {
        snippet: Snippet {
            text: String::new(),
            chars: 0,
            total_lines: 0,
        },
        line_limit,
        cap,
        index: 0,
        started: false,
    };
    collector.snippet.total_lines = scan(reader, &mut collector)?;
    Ok(collector.snippet)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page_collector(start: usize, limit: usize, budget: usize) -> PageCollector {
        PageCollector {
            page: Page {
                content: String::new(),
                line_ends: Vec::new(),
                total_lines: 0,
                oversized_first_line: None,
            },
            start,
            limit,
            budget,
            index: 0,
            chars: 0,
            line: String::new(),
            line_chars: 0,
            stopped: false,
        }
    }
    fn snippet_collector(lines: Option<usize>, cap: usize) -> SnippetCollector {
        SnippetCollector {
            snippet: Snippet {
                text: String::new(),
                chars: 0,
                total_lines: 0,
            },
            line_limit: lines,
            cap,
            index: 0,
            started: false,
        }
    }

    #[test]
    fn text_scan_chunk_sizes_match_string_oracles() {
        let mut inputs = vec![
            "".into(),
            "\n".into(),
            "\n\n".into(),
            "\r".into(),
            "\r\r\n".into(),
            "a\r\nb\rc\n".into(),
            "あ😀e\u{301}\r\n終".into(),
            "a\n\n".into(),
            "long line\nshort\n".into(),
        ];
        inputs.push((0..42).map(|i| format!("{i}😀\r\n")).collect::<String>());
        for text in inputs {
            let lines: Vec<_> = text.lines().collect();
            for chunk in [1, 2, 3, CHUNK_BYTES] {
                for cap in [1, 2, 5, 12, 100] {
                    for start in [0, 1, 2, 100, usize::MAX] {
                        for limit in [1, 2, usize::MAX] {
                            let mut actual = page_collector(start, limit, cap);
                            let count = scan_chunks(text.as_bytes(), &mut actual, chunk).unwrap();
                            assert_eq!(count, lines.len());
                            let mut selected = Vec::new();
                            let mut chars = 0;
                            let mut oversized = None;
                            for line in lines.iter().skip(start).take(limit) {
                                let needed =
                                    line.chars().count() + usize::from(!selected.is_empty());
                                if needed > cap - chars {
                                    if selected.is_empty() {
                                        oversized = Some(line.chars().count());
                                    }
                                    break;
                                }
                                selected.push(*line);
                                chars += needed;
                            }
                            assert_eq!(
                                actual.page.content,
                                selected.join("\n"),
                                "text={text:?}, chunk={chunk}, cap={cap}, start={start}, limit={limit}"
                            );
                            assert_eq!(actual.page.line_ends.len(), selected.len());
                            assert_eq!(actual.page.oversized_first_line, oversized);
                        }
                    }
                    for limit in [None, Some(1), Some(40)] {
                        let mut actual = snippet_collector(limit, cap);
                        let count = scan_chunks(text.as_bytes(), &mut actual, chunk).unwrap();
                        let expected = limit.map_or_else(
                            || text.clone(),
                            |limit| {
                                lines
                                    .iter()
                                    .take(limit)
                                    .copied()
                                    .collect::<Vec<_>>()
                                    .join("\n")
                            },
                        );
                        assert_eq!(count, lines.len());
                        assert_eq!(actual.snippet.chars, expected.chars().count());
                        assert_eq!(
                            actual.snippet.text,
                            expected.chars().take(cap).collect::<String>()
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn text_scan_validates_unselected_tail_and_propagates_io_errors() {
        for bytes in [
            b"ok\n\xff".as_slice(),
            b"ok\n\xf0\x9f\x98",
            b"\xed\xa0\x80",
            b"\xc0\xaf",
        ] {
            for chunk in [1, 2, 3, CHUNK_BYTES] {
                let mut visitor = page_collector(0, 1, 8);
                assert_eq!(
                    scan_chunks(bytes, &mut visitor, chunk).unwrap_err().kind(),
                    io::ErrorKind::InvalidData
                );
                let mut visitor = snippet_collector(Some(1), 8);
                assert_eq!(
                    scan_chunks(bytes, &mut visitor, chunk).unwrap_err().kind(),
                    io::ErrorKind::InvalidData
                );
            }
        }
        struct FailAfterPage(bool);
        impl Read for FailAfterPage {
            fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
                if self.0 {
                    return Err(io::Error::other("fixture I/O failure"));
                }
                self.0 = true;
                bytes[..3].copy_from_slice(b"ok\n");
                Ok(3)
            }
        }
        assert!(
            read_page(FailAfterPage(false), 0, 1, 8)
                .unwrap_err()
                .to_string()
                .contains("fixture I/O failure")
        );
        assert!(
            read_snippet(FailAfterPage(false), Some(1), 8)
                .unwrap_err()
                .to_string()
                .contains("fixture I/O failure")
        );
    }

    struct Repeated {
        byte: u8,
        remaining: usize,
    }
    impl Read for Repeated {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            let count = bytes.len().min(self.remaining);
            bytes[..count].fill(self.byte);
            self.remaining -= count;
            Ok(count)
        }
    }
    struct MeasuredPage {
        collector: PageCollector,
        peak: usize,
    }
    impl MeasuredPage {
        fn measure(&mut self) {
            self.peak = self.peak.max(
                self.collector.page.content.capacity()
                    + self.collector.line.capacity()
                    + self.collector.page.line_ends.capacity() * size_of::<usize>(),
            );
        }
    }
    impl Visitor for MeasuredPage {
        fn line_char(&mut self, ch: char) {
            self.collector.line_char(ch);
            self.measure();
        }
        fn end_line(&mut self, index: usize) {
            self.collector.end_line(index);
            self.measure();
        }
    }
    #[test]
    fn text_scan_large_logical_inputs_keep_collector_capacity_bounded() {
        let len = 16 * 1024 * 1024;
        for (byte, start, expected_lines) in [(b'x', 0, 1), (b'x', 1, 1), (b'\n', 0, len)] {
            let mut measured = MeasuredPage {
                collector: page_collector(start, usize::MAX, 64),
                peak: 0,
            };
            assert_eq!(
                scan(
                    Repeated {
                        byte,
                        remaining: len
                    },
                    &mut measured
                )
                .unwrap(),
                expected_lines
            );
            assert!(
                measured.peak <= 4096,
                "retained capacity grew: {}",
                measured.peak
            );
            assert!(measured.collector.page.content.chars().count() <= 64);
            if byte == b'x' && start == 0 {
                assert_eq!(measured.collector.page.oversized_first_line, Some(len));
            }
        }
        for mode in [None, Some(40)] {
            let result = read_snippet(
                Repeated {
                    byte: b'x',
                    remaining: len,
                },
                mode,
                64,
            )
            .unwrap();
            assert_eq!(result.chars, len);
            assert_eq!(result.text.len(), 64);
            assert!(result.text.capacity() <= 128);
        }
        // Discard a giant nonselected line without keeping its bytes, then select a Unicode line.
        let input = Repeated {
            byte: b'x',
            remaining: len,
        }
        .chain(b"\n\xe3\x81\x82\r\n".as_slice());
        let page = read_page(input, 1, 1, 1).unwrap();
        assert_eq!(page.content, "あ");
        assert_eq!(page.total_lines, 2);
        assert_eq!(page.line_ends.len(), 1);
    }

    #[test]
    fn text_scan_retries_interrupted_reads() {
        struct Interrupted {
            first: bool,
            bytes: io::Cursor<Vec<u8>>,
        }
        impl Read for Interrupted {
            fn read(&mut self, target: &mut [u8]) -> io::Result<usize> {
                if self.first {
                    self.first = false;
                    return Err(io::ErrorKind::Interrupted.into());
                }
                self.bytes.read(target)
            }
        }
        assert_eq!(
            read_snippet(
                Interrupted {
                    first: true,
                    bytes: io::Cursor::new("😀\r\n".as_bytes().to_vec())
                },
                Some(40),
                1
            )
            .unwrap()
            .text,
            "😀"
        );
    }
}
