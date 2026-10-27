//! A bounded structural pre-scan of an imported audio file, run before symphonia probes it.
//!
//! symphonia 0.5 sizes several allocations by a count or length it reads from the file, before
//! reading any of the bytes that count describes: an MP4 sample table's entry count, a CAF packet
//! table's packet count or magic cookie size, a FLAC picture's lengths, a Vorbis comment's
//! length. A few-KB file declaring billions asks for tens of gigabytes during the probe itself,
//! and an allocation failure aborts the process. It is not a panic, so the FFI's catch_unwind
//! cannot stop it, and the import gate probes too.
//!
//! The rule here is the one those readers skip: no count or length may exceed the bytes left to
//! hold it. A file that breaks it is refused, and everything else is left for symphonia to judge,
//! including what this scan cannot make sense of. The scan follows the readers' own paths through
//! the file: which container the probe settles on, which atoms and chunks the reader descends
//! into, and where it resumes after each. Walking somewhere the reader does not go could refuse a
//! good file, and skipping somewhere it does go would let a bad one through. Only headers and
//! metadata are read, never the audio, within fixed bounds.
//!
//! MP3, WAV, AIFF and ADTS need no scan: their readers' allocations are bounded or capped.

use std::io::{self, Read, Seek, SeekFrom};

/// Why [`screen`] refused a file.
#[derive(Debug)]
pub enum Refusal {
    /// The file declares a count or length that its own bytes cannot hold, or a structure too
    /// large to check within the scan's bounds.
    Impossible(String),
    /// The file could not be read.
    Io(io::Error),
}

impl std::fmt::Display for Refusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Refusal::Impossible(why) => f.write_str(why),
            Refusal::Io(e) => write!(f, "read: {e}"),
        }
    }
}

impl From<io::Error> for Refusal {
    fn from(e: io::Error) -> Self {
        Refusal::Io(e)
    }
}

fn impossible<T>(why: String) -> Result<T, Refusal> {
    Err(Refusal::Impossible(why))
}

/// Refuses `src` when the container symphonia would read it as declares a table, chunk or
/// metadata length that the file cannot hold. Leaves `src` where it found it.
pub fn screen<R: Read + Seek>(src: &mut R) -> Result<(), Refusal> {
    let start = src.stream_position()?;
    let verdict = scan(&mut Source::new(&mut *src)?);
    src.seek(SeekFrom::Start(start))?;
    verdict
}

/// How far symphonia's probe looks for a container marker before giving up (its own constant).
const PROBE_SEARCH_LIMIT: u64 = 1 << 20;

/// ID3v2 tags followed to the container behind them. The probe follows any number, so stopping
/// short would let a malformed container through behind enough of them; real files carry one.
const MAX_ID3_TAGS: usize = 16;

fn scan<R: Read + Seek>(file: &mut Source<R>) -> Result<(), Refusal> {
    let mut from = 0;
    for _ in 0..=MAX_ID3_TAGS {
        let Some((at, marker)) = find_marker(file, from)? else {
            return Ok(());
        };
        match marker {
            // The probe reads the tag and resumes its search after it. Followed whether or not
            // the reader would accept the tag: following one it rejects only scans more.
            Marker::Id3 => {
                let Some(size) = id3_size(file, at)? else {
                    return Ok(());
                };
                from = at + 10 + size;
            }
            // The reader starts at the atom whose type the marker is.
            Marker::Mp4 => return mp4(file, at.saturating_sub(4)),
            Marker::Caf => return caf(file, at),
            Marker::Flac => return flac(file, at),
            Marker::Ogg => return ogg(file, at),
            Marker::Unscreened => return Ok(()),
        }
    }
    impossible(format!("more than {MAX_ID3_TAGS} ID3v2 tags"))
}

#[derive(Clone, Copy, Debug, PartialEq)]
enum Marker {
    Mp4,
    Caf,
    Flac,
    Ogg,
    Id3,
    /// MP3, ADTS, WAV or AIFF.
    Unscreened,
}

/// Second bytes after 0xFF that the probe takes as an MPEG audio frame (all three layers,
/// registered whatever the build's features) or an ADTS header.
const MPEG_SYNC: &[u8] = &[
    0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xfa, 0xfb, 0xfc,
    0xfd, 0xfe, 0xff,
];

fn marker(head: &[u8]) -> Option<Marker> {
    match head {
        [0xff, second, ..] if MPEG_SYNC.contains(second) => Some(Marker::Unscreened),
        [b'R', b'I', b'F', b'F', ..] | [b'F', b'O', b'R', b'M', ..] => Some(Marker::Unscreened),
        [b'f', b't', b'y', b'p', ..] => Some(Marker::Mp4),
        [b'c', b'a', b'f', b'f', ..] => Some(Marker::Caf),
        [b'f', b'L', b'a', b'C', ..] => Some(Marker::Flac),
        [b'O', b'g', b'g', b'S', ..] => Some(Marker::Ogg),
        [b'I', b'D', b'3', ..] => Some(Marker::Id3),
        _ => None,
    }
}

/// Where the probe would find its next marker at or after `from`, and which. It tries every
/// position of the next MiB in turn and takes the first registered marker there, whatever the
/// extension: a valid container behind a few junk bytes is still found and read.
fn find_marker<R: Read + Seek>(
    file: &mut Source<R>,
    from: u64,
) -> io::Result<Option<(u64, Marker)>> {
    let mut head = [0u8; 4];
    for at in from..from + PROBE_SEARCH_LIMIT - 1 {
        let n = file.read_upto(at, &mut head)?;
        if n < 2 {
            break;
        }
        if let Some(m) = marker(&head[..n]) {
            return Ok(Some((at, m)));
        }
    }
    Ok(None)
}

/// The size an ID3v2 header at `at` gives its tag body (28 bits, seven to a byte).
fn id3_size<R: Read + Seek>(file: &mut Source<R>, at: u64) -> io::Result<Option<u64>> {
    let mut head = [0u8; 10];
    if !file.read_at(at, &mut head)? {
        return Ok(None);
    }
    Ok(Some(
        head[6..10]
            .iter()
            .fold(0u64, |size, &b| (size << 7) | u64::from(b & 0x7f)),
    ))
}

// ---------------------------------------------------------------------------------------------
// MP4 / QuickTime

/// Atoms walked before giving up. A fragmented file holds two top-level atoms per fragment, so
/// this is hours of audio; a file with more is refused rather than half checked.
const MAX_ATOMS: u32 = 1 << 16;

/// An atom whose children the isomp4 reader parses, on its way to the sample tables and the
/// iTunes metadata: the only atoms whose contents it sizes allocations by. Each kind nests only
/// in the one before it, so the walk is never more than six deep.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Atom {
    File,
    Moov,
    Trak,
    Mdia,
    Minf,
    Stbl,
    Udta,
    Meta,
    Ilst,
    /// An iTunes metadata item, whose `data`, `name` and `mean` children are read whole into
    /// buffers sized by their headers.
    Item,
}

fn mp4<R: Read + Seek>(file: &mut Source<R>, root: u64) -> Result<(), Refusal> {
    // Each level: what it is, where its next child starts, where it ends.
    let mut levels = vec![(Atom::File, root, file.len)];
    let mut atoms = 0;
    while let Some(&(parent, at, end)) = levels.last() {
        let mut head = [0u8; 8];
        if end - at < 8 || !file.read_at(at, &mut head)? {
            levels.pop();
            continue;
        }
        atoms += 1;
        if atoms > MAX_ATOMS {
            return impossible(format!("mp4: more than {MAX_ATOMS} atoms"));
        }
        let kind = [head[4], head[5], head[6], head[7]];
        let (size, header) = match u32::from_be_bytes([head[0], head[1], head[2], head[3]]) {
            // To the end of the parent (or of the file).
            0 => (end - at, 8),
            // A 64-bit size follows the type.
            1 => match file.u64_be(at + 8)? {
                Some(size) if size >= 16 => (size, 16),
                _ => return Ok(()),
            },
            size if size >= 8 => (u64::from(size), 8),
            // The reader refuses an atom shorter than its own header.
            _ => return Ok(()),
        };
        if size > end - at {
            // A recording cut short leaves its mdat running past the end, and the reader still
            // plays what is there. Nothing after a top-level atom like that is read, but a moov
            // or meta that overruns is parsed, by sizes that cannot be true.
            if parent == Atom::File && &kind != b"moov" && &kind != b"meta" {
                return Ok(());
            }
            return impossible(format!(
                "mp4: '{}' atom of {size} bytes where {} remain",
                fourcc(kind),
                end - at
            ));
        }
        if let Some(level) = levels.last_mut() {
            level.1 = at + size;
        }
        let (body, body_end) = (at + header, at + size);
        let child = match (parent, &kind) {
            (Atom::File, b"moov") => Some(Atom::Moov),
            (Atom::File, b"meta") | (Atom::Udta, b"meta") => Some(Atom::Meta),
            (Atom::Moov, b"trak") => Some(Atom::Trak),
            (Atom::Moov, b"udta") => Some(Atom::Udta),
            (Atom::Trak, b"mdia") => Some(Atom::Mdia),
            (Atom::Mdia, b"minf") => Some(Atom::Minf),
            (Atom::Minf, b"stbl") => Some(Atom::Stbl),
            (Atom::Meta, b"ilst") => Some(Atom::Ilst),
            (Atom::Ilst, _) => Some(Atom::Item),
            (Atom::Stbl, _) => {
                sample_table(file, kind, body, body_end)?;
                None
            }
            _ => None,
        };
        match child {
            // The reader skips a meta atom's version and flags before its children, and takes
            // the length left as unsigned.
            Some(Atom::Meta) if body_end - body < 4 => {
                return impossible(format!("mp4: 'meta' atom of {size} bytes"));
            }
            Some(Atom::Meta) => levels.push((Atom::Meta, body + 4, body_end)),
            Some(atom) => levels.push((atom, body, body_end)),
            None => {}
        }
    }
    Ok(())
}

/// Refuses a sample table whose entry count needs more bytes than the atom holds after it: the
/// reader reserves the whole table from the count alone.
fn sample_table<R: Read + Seek>(
    file: &mut Source<R>,
    kind: [u8; 4],
    body: u64,
    end: u64,
) -> Result<(), Refusal> {
    // Bytes ahead of the count (version and flags, then for stsz the constant sample size), and
    // an entry's size on disk.
    let (before, stride) = match &kind {
        b"stco" => (4, 4),
        b"co64" | b"stts" => (4, 8),
        b"stsc" => (4, 12),
        // A table only when no constant sample size precedes the count.
        b"stsz" if end - body >= 8 && file.u32_be(body + 4)? != Some(0) => return Ok(()),
        b"stsz" => (8, 4),
        _ => return Ok(()),
    };
    let fixed = before + 4;
    // Short of its count, the reader takes one from whatever bytes follow the atom.
    let count = match file.u32_be(body + before)? {
        Some(count) if end - body >= fixed => u64::from(count),
        _ => {
            return impossible(format!(
                "mp4: '{}' atom too short for its entry count",
                fourcc(kind)
            ))
        }
    };
    let room = end - body - fixed;
    if count * stride > room {
        return impossible(format!(
            "mp4: '{}' declares {count} entries of {stride} bytes in {room}",
            fourcc(kind)
        ));
    }
    Ok(())
}

fn fourcc(kind: [u8; 4]) -> String {
    kind.iter()
        .map(|&b| if b.is_ascii_graphic() { b as char } else { '?' })
        .collect()
}

// ---------------------------------------------------------------------------------------------
// CAF

/// Chunks walked before giving up; real files have a handful.
const MAX_CAF_CHUNKS: u32 = 1 << 12;

/// The frames per packet symphonia's CAF reader reads uncompressed audio in (its own constant).
const CAF_PCM_PACKET_FRAMES: u64 = 1152;

/// The fields of a CAF audio description the packet table is read by.
#[derive(Clone, Copy)]
struct CafFormat {
    bytes_per_packet: u32,
    frames_per_packet: u32,
    bits_per_channel: u32,
}

fn caf<R: Read + Seek>(file: &mut Source<R>, start: u64) -> Result<(), Refusal> {
    // 'caff', version 1, flags.
    if file.u16_be(start + 4)? != Some(1) {
        return Ok(());
    }
    let len = file.len;
    let mut at = start + 8;
    let mut format = None;
    let mut unsized_data = false;
    let mut chunks = 0;
    // The reader reads chunk after chunk until it stands exactly at the end of the file. Where
    // it resumes after a chunk is where its parse of that chunk stopped, not always where the
    // chunk's size says it ends, and this walk resumes there too.
    while at < len {
        if chunks == MAX_CAF_CHUNKS {
            return impossible(format!("caf: more than {MAX_CAF_CHUNKS} chunks"));
        }
        chunks += 1;
        let mut head = [0u8; 12];
        if !file.read_at(at, &mut head)? {
            return Ok(());
        }
        let kind = [head[0], head[1], head[2], head[3]];
        let size = i64::from_be_bytes([
            head[4], head[5], head[6], head[7], head[8], head[9], head[10], head[11],
        ]);
        let body = at + 12;
        let left = len - body;
        // The audio description must come first; the reader refuses anything else there.
        if chunks == 1 && &kind != b"desc" {
            return Ok(());
        }
        at = match &kind {
            b"desc" => {
                let mut desc = [0u8; 32];
                if size != 32 || format.is_some() || !file.read_at(body, &mut desc)? {
                    return Ok(());
                }
                let field =
                    |i: usize| u32::from_be_bytes([desc[i], desc[i + 1], desc[i + 2], desc[i + 3]]);
                format = Some(CafFormat {
                    bytes_per_packet: field(12),
                    frames_per_packet: field(16),
                    bits_per_channel: field(24),
                });
                body + 32
            }
            // A size of -1 is a data chunk still being written, running to the end of the file.
            // The reader then carries on reading chunks from the start of the audio.
            b"data" if size == -1 => {
                unsized_data = true;
                body + 4
            }
            b"pakt" => {
                let Some(format) = format.filter(|_| size >= 24) else {
                    return Ok(());
                };
                let (Some(packets), Some(frames)) = (file.i64_be(body)?, file.i64_be(body + 8)?)
                else {
                    return Ok(());
                };
                if packets < 0 || frames < 0 {
                    return Ok(());
                }
                // Every entry takes at least a byte, and the reader reserves the whole table
                // (32 bytes an entry) from the count alone.
                let room = (size as u64).min(left).saturating_sub(24);
                if packets as u64 > room {
                    return impossible(format!(
                        "caf: packet table declares {packets} packets in {room} bytes"
                    ));
                }
                match packet_table(file, body + 24, packets as u64, format)? {
                    Some(end) => end,
                    None => return Ok(()),
                }
            }
            // The magic cookie is read whole, into a buffer of its declared size.
            b"kuki" if size >= 0 && size as u64 > left => {
                return impossible(format!(
                    "caf: magic cookie of {size} bytes where {left} remain"
                ));
            }
            b"chan" => {
                let Some(count) = file.u32_be(body + 8)?.filter(|_| size >= 12) else {
                    return Ok(());
                };
                // Twenty bytes a channel description, read whatever the chunk's size.
                body + 12 + 20 * u64::from(count)
            }
            // Every other chunk is skipped by its size, and a negative one is refused.
            _ if size >= 0 => body + size as u64,
            _ => return Ok(()),
        };
    }
    if at > len {
        return Ok(());
    }
    // With the data chunk's size unknown, uncompressed audio is read in packets of 1152 frames,
    // each into a buffer sized from the declared bytes per frame, with no end of data to cap it.
    if let Some(format) = format.filter(|f| unsized_data && f.bits_per_channel != 0) {
        let packet = u64::from(format.bytes_per_packet) * CAF_PCM_PACKET_FRAMES;
        if packet > len {
            return impossible(format!("caf: packets of {packet} bytes in a file of {len}"));
        }
    }
    Ok(())
}

/// Walks the entries of a CAF packet table from `at` as the reader parses them (each a
/// variable-length size and/or frame count, as the format leaves them variable) and returns
/// where they end, or None where they run off the file or a number does not terminate. Every
/// packet is later read into a buffer of its declared size, so one larger than the whole file is
/// refused.
fn packet_table<R: Read + Seek>(
    file: &mut Source<R>,
    mut at: u64,
    packets: u64,
    format: CafFormat,
) -> Result<Option<u64>, Refusal> {
    let sizes = format.bytes_per_packet == 0;
    let frames = format.frames_per_packet == 0;
    if !sizes && !frames {
        // A constant-rate format has no table; the reader refuses entries.
        return Ok(if packets == 0 { Some(at) } else { None });
    }
    for _ in 0..packets {
        if sizes {
            let Some(size) = varint(file, &mut at)? else {
                return Ok(None);
            };
            if size > file.len {
                return impossible(format!(
                    "caf: packet of {size} bytes in a file of {}",
                    file.len
                ));
            }
        }
        if frames && varint(file, &mut at)?.is_none() {
            return Ok(None);
        }
    }
    Ok(Some(at))
}

/// A CAF variable-length integer at `at` (seven bits a byte, at most nine bytes), advancing past
/// it.
fn varint<R: Read + Seek>(file: &mut Source<R>, at: &mut u64) -> io::Result<Option<u64>> {
    let mut value = 0u64;
    for _ in 0..9 {
        let mut b = [0u8];
        if !file.read_at(*at, &mut b)? {
            return Ok(None);
        }
        *at += 1;
        value = (value << 7) | u64::from(b[0] & 0x7f);
        if b[0] & 0x80 == 0 {
            return Ok(Some(value));
        }
    }
    Ok(None)
}

// ---------------------------------------------------------------------------------------------
// FLAC

/// Metadata blocks walked before giving up; real files have a handful.
const MAX_FLAC_BLOCKS: u32 = 1 << 12;

fn flac<R: Read + Seek>(file: &mut Source<R>, start: u64) -> Result<(), Refusal> {
    let mut at = start + 4;
    for _ in 0..MAX_FLAC_BLOCKS {
        let mut head = [0u8; 4];
        if !file.read_at(at, &mut head)? {
            return Ok(());
        }
        let len = u64::from(u32::from_be_bytes([0, head[1], head[2], head[3]]));
        let body = at + 4;
        // Read through a window of the block's length, past which the reader will not read.
        let size = len.min(file.len - body);
        let mut block = FileWindow {
            file: &mut *file,
            start: body,
            size,
        };
        match head[0] & 0x7f {
            4 => {
                vorbis_comments(&mut block)?;
            }
            6 => {
                picture(&mut block)?;
            }
            _ => {}
        }
        if head[0] & 0x80 != 0 {
            return Ok(());
        }
        at = body + len;
    }
    impossible(format!("flac: more than {MAX_FLAC_BLOCKS} metadata blocks"))
}

// ---------------------------------------------------------------------------------------------
// Ogg

/// Page bytes scanned, and pages, before the scan stops and leaves the file to symphonia. Twice
/// the largest packet the reader assembles, so a cover image ahead of the comments cannot push
/// them out of reach.
const MAX_OGG_BYTES: u64 = 32 << 20;
const MAX_OGG_PAGES: u32 = 1 << 17;

/// The largest packet symphonia's Ogg reader assembles before giving up on it (its own
/// constant).
const MAX_OGG_PACKET: usize = 16 << 20;

/// How symphonia maps a logical stream's packets, chosen from its first one.
#[derive(Clone, Copy, PartialEq)]
enum Mapping {
    Vorbis,
    Opus,
    Flac,
}

/// One logical stream, assembled into packets as symphonia's reader assembles them.
struct Stream {
    serial: u32,
    mapping: Mapping,
    partial: Vec<u8>,
    last_sequence: Option<u32>,
    /// An audio packet came through, so its header packets are behind it.
    audio: bool,
    /// Opus: the comment header was read (until then, each packet is tried as one).
    tags_read: bool,
}

struct OggPage {
    serial: u32,
    sequence: u32,
    first: bool,
    continued: bool,
}

/// Checks the header packets of each logical stream in the first link of an Ogg file: the
/// comment packet of a Vorbis or Opus stream, and the comment and picture blocks of an Ogg FLAC
/// one. Bounded, so a stream whose headers outlast the bounds is left unchecked.
fn ogg<R: Read + Seek>(file: &mut Source<R>, start: u64) -> Result<(), Refusal> {
    let mut pages = Pages {
        at: start,
        scanned: 0,
        read: 0,
        segments: Vec::new(),
        body: Vec::new(),
    };
    // The reader gives up on a stream whose first page is damaged or does not open a stream.
    let mut page = match pages.next(file)? {
        Next::Page(page) if page.first => page,
        _ => return Ok(()),
    };
    let mut streams: Vec<Stream> = Vec::new();
    // Each opening page carries a stream's identification packet, which picks its mapping.
    while page.first {
        if let Some(&(_, len)) = pages.packets().first() {
            if let Some(mapping) = mapping(&pages.body[..len]) {
                streams.retain(|s| s.serial != page.serial);
                streams.push(Stream {
                    serial: page.serial,
                    mapping,
                    partial: Vec::new(),
                    last_sequence: None,
                    audio: false,
                    tags_read: false,
                });
            }
        }
        page = match pages.next_valid(file)? {
            Some(next) => next,
            None => return Ok(()),
        };
    }
    loop {
        // A stream opening now is the next link of a chained file, beyond this scan.
        if page.first || streams.iter().all(|s| s.audio) {
            return Ok(());
        }
        if let Some(stream) = streams.iter_mut().find(|s| s.serial == page.serial) {
            if !stream.read_page(&page, &pages)? {
                return Ok(());
            }
        }
        page = match pages.next_valid(file)? {
            Some(next) => next,
            None => return Ok(()),
        };
    }
}

fn mapping(packet: &[u8]) -> Option<Mapping> {
    if packet.len() == 51 && packet.starts_with(b"\x7fFLAC") {
        Some(Mapping::Flac)
    } else if packet.len() == 30 && packet.starts_with(b"\x01vorbis") {
        Some(Mapping::Vorbis)
    } else if packet.len() >= 19 && packet.starts_with(b"OpusHead") {
        Some(Mapping::Opus)
    } else {
        None
    }
}

impl Stream {
    /// Takes in a page as the reader's logical stream does: dropping partial data across a gap
    /// in the sequence or a missing continuation, and mapping each packet completed. False once
    /// a packet outgrows what the reader will assemble (it fails there).
    fn read_page(&mut self, page: &OggPage, pages: &Pages) -> Result<bool, Refusal> {
        if let Some(last) = self.last_sequence {
            if page.sequence < last || page.sequence - last > 1 {
                self.partial.clear();
            }
        }
        self.last_sequence = Some(page.sequence);
        if !page.continued && !self.partial.is_empty() {
            self.partial.clear();
        }
        let packets = pages.packets();
        let mut skip = 0;
        if page.continued && self.partial.is_empty() {
            if packets.is_empty() {
                return Ok(true);
            }
            skip = 1;
        }
        let mut end = 0;
        for &(at, len) in &packets {
            end = at + len;
        }
        for &(at, len) in &packets[skip..] {
            let piece = &pages.body[at..at + len];
            if self.partial.is_empty() {
                self.map(piece)?;
            } else {
                self.partial.extend_from_slice(piece);
                let packet = std::mem::take(&mut self.partial);
                self.map(&packet)?;
            }
        }
        let rest = &pages.body[end..];
        if !rest.is_empty() {
            if self.partial.len() + rest.len() > MAX_OGG_PACKET {
                return Ok(false);
            }
            self.partial.extend_from_slice(rest);
        }
        Ok(true)
    }

    fn map(&mut self, packet: &[u8]) -> Result<(), Refusal> {
        match self.mapping {
            Mapping::Vorbis => match packet.first() {
                Some(kind) if kind & 1 == 0 => self.audio = true,
                // The comment header, after its type and signature.
                Some(3) if packet.get(1..7) == Some(b"vorbis") => {
                    vorbis_comments(&mut SliceWindow(&packet[7..]))?;
                }
                _ => {}
            },
            Mapping::Opus if self.tags_read => self.audio = true,
            Mapping::Opus => {
                // The reader takes packets as the comment header until one parses as it.
                if packet.starts_with(b"OpusTags")
                    && vorbis_comments(&mut SliceWindow(&packet[8..]))? == Walk::Done
                {
                    self.tags_read = true;
                }
            }
            Mapping::Flac => match packet.first() {
                Some(0xff) => self.audio = true,
                Some(0x00 | 0x80) | None => {}
                // Any other packet is a metadata block, read to the end of the packet whatever
                // length its header gives.
                Some(kind) if packet.len() >= 4 => match kind & 0x7f {
                    4 => {
                        vorbis_comments(&mut SliceWindow(&packet[4..]))?;
                    }
                    6 => {
                        picture(&mut SliceWindow(&packet[4..]))?;
                    }
                    _ => {}
                },
                Some(_) => {}
            },
        }
        Ok(())
    }
}

enum Next {
    Page(OggPage),
    /// A page the reader would skip: a malformed header or a checksum mismatch.
    Damaged,
    End,
}

/// Ogg pages read as symphonia's page reader reads them: found by their capture pattern, with
/// their checksum checked, the reader resuming its search where it does after a bad page.
struct Pages {
    at: u64,
    scanned: u64,
    read: u32,
    segments: Vec<u8>,
    body: Vec<u8>,
}

impl Pages {
    fn next_valid<R: Read + Seek>(&mut self, file: &mut Source<R>) -> io::Result<Option<OggPage>> {
        loop {
            match self.next(file)? {
                Next::Page(page) => return Ok(Some(page)),
                Next::Damaged => {}
                Next::End => return Ok(None),
            }
        }
    }

    fn next<R: Read + Seek>(&mut self, file: &mut Source<R>) -> io::Result<Next> {
        let Some(sync) = self.sync(file)? else {
            return Ok(Next::End);
        };
        let mut head = [0u8; 27];
        if !file.read_at(sync, &mut head)? {
            return Ok(Next::End);
        }
        self.read += 1;
        self.scanned += 27;
        // Past a malformed header, the reader resumes its search after it.
        self.at = sync + 27;
        if head[4] != 0 || head[5] & 0xf8 != 0 {
            return Ok(Next::Damaged);
        }
        self.segments.resize(usize::from(head[26]), 0);
        if !file.read_at(sync + 27, &mut self.segments)? {
            return Ok(Next::End);
        }
        let body_len: usize = self.segments.iter().map(|&s| usize::from(s)).sum();
        self.body.resize(body_len, 0);
        let body_at = sync + 27 + self.segments.len() as u64;
        if !file.read_at(body_at, &mut self.body)? {
            return Ok(Next::End);
        }
        self.scanned += self.segments.len() as u64 + body_len as u64;
        let stored = u32::from_le_bytes([head[22], head[23], head[24], head[25]]);
        head[22..26].fill(0);
        let sum = ogg_crc(ogg_crc(ogg_crc(0, &head), &self.segments), &self.body);
        if sum != stored {
            // The reader resumes its search just past this page's capture pattern.
            self.at = sync + 4;
            return Ok(Next::Damaged);
        }
        self.at = body_at + body_len as u64;
        Ok(Next::Page(OggPage {
            serial: u32::from_le_bytes([head[14], head[15], head[16], head[17]]),
            sequence: u32::from_le_bytes([head[18], head[19], head[20], head[21]]),
            first: head[5] & 0x02 != 0,
            continued: head[5] & 0x01 != 0,
        }))
    }

    /// The next capture pattern at or after `at`, within the scan's bounds.
    fn sync<R: Read + Seek>(&mut self, file: &mut Source<R>) -> io::Result<Option<u64>> {
        let mut chunk = [0u8; 4096];
        loop {
            if self.scanned >= MAX_OGG_BYTES || self.read >= MAX_OGG_PAGES {
                return Ok(None);
            }
            let n = file.read_upto(self.at, &mut chunk)?;
            if n < 4 {
                return Ok(None);
            }
            if let Some(i) = chunk[..n].windows(4).position(|w| w == b"OggS") {
                self.scanned += i as u64;
                return Ok(Some(self.at + i as u64));
            }
            // The last three bytes may begin a pattern the next read completes.
            self.at += n as u64 - 3;
            self.scanned += n as u64 - 3;
        }
    }

    /// The current page's complete packets, as (offset, length) in its body. Whatever follows
    /// the last of them is a packet continuing on a later page.
    fn packets(&self) -> Vec<(usize, usize)> {
        let mut packets = Vec::new();
        let (mut at, mut len) = (0, 0);
        for &segment in &self.segments {
            len += usize::from(segment);
            if segment < 255 {
                packets.push((at, len));
                at += len;
                len = 0;
            }
        }
        packets
    }
}

/// The Ogg page checksum: CRC-32, polynomial 0x04C11DB7, unreflected, zero initial value.
fn ogg_crc(mut crc: u32, bytes: &[u8]) -> u32 {
    const TABLE: [u32; 256] = {
        let mut table = [0u32; 256];
        let mut i = 0;
        while i < 256 {
            let mut c = (i as u32) << 24;
            let mut bit = 0;
            while bit < 8 {
                c = if c & 0x8000_0000 != 0 {
                    (c << 1) ^ 0x04c1_1db7
                } else {
                    c << 1
                };
                bit += 1;
            }
            table[i] = c;
            i += 1;
        }
        table
    };
    for &b in bytes {
        crc = (crc << 8) ^ TABLE[usize::from((crc >> 24) as u8 ^ b)];
    }
    crc
}

// ---------------------------------------------------------------------------------------------
// Vorbis comments and FLAC pictures, shared by FLAC and Ogg

/// How far a structure's walk got without finding an impossible length.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Walk {
    Done,
    /// The bytes ran out first: the reader fails there, before sizing anything further.
    Short,
}

/// A run of bytes one structure is read from: a FLAC metadata block in the file, an Ogg packet
/// in memory, or the picture a comment carries in base64.
trait Window {
    fn size(&self) -> u64;
    /// Fills `out` from offset `at`; false when the window ends first or its bytes cannot be
    /// decoded.
    fn read(&mut self, at: u64, out: &mut [u8]) -> io::Result<bool>;
}

struct FileWindow<'a, R> {
    file: &'a mut Source<R>,
    start: u64,
    size: u64,
}

impl<R: Read + Seek> Window for FileWindow<'_, R> {
    fn size(&self) -> u64 {
        self.size
    }

    fn read(&mut self, at: u64, out: &mut [u8]) -> io::Result<bool> {
        if at + out.len() as u64 > self.size {
            return Ok(false);
        }
        self.file.read_at(self.start + at, out)
    }
}

struct SliceWindow<'a>(&'a [u8]);

impl Window for SliceWindow<'_> {
    fn size(&self) -> u64 {
        self.0.len() as u64
    }

    fn read(&mut self, at: u64, out: &mut [u8]) -> io::Result<bool> {
        match usize::try_from(at)
            .ok()
            .and_then(|at| self.0.get(at..at.checked_add(out.len())?))
        {
            Some(bytes) => {
                out.copy_from_slice(bytes);
                Ok(true)
            }
            None => Ok(false),
        }
    }
}

/// The bytes a base64 value decodes to, decoded as symphonia's decoder does (trailing '='
/// trimmed, a final group of two or three symbols yielding one or two bytes) but only where
/// read. A symbol outside the alphabet makes the whole value undecodable, so nothing is read
/// from it.
struct Base64<'a> {
    text: &'a mut dyn Window,
    start: u64,
    symbols: u64,
}

impl<'a> Base64<'a> {
    fn new(text: &'a mut dyn Window, start: u64, len: u64) -> io::Result<Self> {
        let mut symbols = len;
        let mut tail = [0u8; 64];
        while symbols > 0 {
            let n = symbols.min(64) as usize;
            if !text.read(start + symbols - n as u64, &mut tail[..n])? {
                break;
            }
            let padding = tail[..n].iter().rev().take_while(|&&b| b == b'=').count();
            symbols -= padding as u64;
            if padding < n {
                break;
            }
        }
        Ok(Base64 {
            text,
            start,
            symbols,
        })
    }
}

impl Window for Base64<'_> {
    fn size(&self) -> u64 {
        self.symbols / 4 * 3 + (self.symbols % 4).saturating_sub(1)
    }

    fn read(&mut self, at: u64, out: &mut [u8]) -> io::Result<bool> {
        for (i, byte) in out.iter_mut().enumerate() {
            let k = at + i as u64;
            if k >= self.size() {
                return Ok(false);
            }
            let first = k / 3 * 4;
            let n = (self.symbols - first).min(4) as usize;
            let mut group = [0u8; 4];
            if !self.text.read(self.start + first, &mut group[..n])? {
                return Ok(false);
            }
            let mut v = [0u8; 4];
            for (v, &symbol) in v.iter_mut().zip(&group[..n]) {
                *v = match symbol {
                    b'A'..=b'Z' => symbol - b'A',
                    b'a'..=b'z' => symbol - b'a' + 26,
                    b'0'..=b'9' => symbol - b'0' + 52,
                    b'+' => 62,
                    b'/' => 63,
                    _ => return Ok(false),
                };
            }
            *byte = match k % 3 {
                0 => (v[0] << 2) | (v[1] >> 4),
                1 => (v[1] << 4) | (v[2] >> 2),
                _ => (v[2] << 6) | v[3],
            };
        }
        Ok(true)
    }
}

/// Refuses `what` declaring `need` bytes when fewer than that follow `at` in `w`.
fn room(w: &dyn Window, at: u64, need: u64, what: &str) -> Result<(), Refusal> {
    let left = w.size().saturating_sub(at);
    if need > left {
        return impossible(format!("{what} of {need} bytes where {left} remain"));
    }
    Ok(())
}

fn u32_le(w: &mut dyn Window, at: u64) -> io::Result<Option<u32>> {
    let mut b = [0u8; 4];
    Ok(w.read(at, &mut b)?.then(|| u32::from_le_bytes(b)))
}

fn u32_be(w: &mut dyn Window, at: u64) -> io::Result<Option<u32>> {
    let mut b = [0u8; 4];
    Ok(w.read(at, &mut b)?.then(|| u32::from_be_bytes(b)))
}

/// Walks a Vorbis comment list (a vendor string, a count, then the comments, each after its
/// little-endian length) from the start of `w`. The reader reserves each comment's buffer from
/// its length before reading it, and decodes a `METADATA_BLOCK_PICTURE` comment's base64 into a
/// FLAC picture, whose lengths it trusts the same way.
fn vorbis_comments(w: &mut dyn Window) -> Result<Walk, Refusal> {
    let Some(vendor) = u32_le(w, 0)? else {
        return Ok(Walk::Short);
    };
    let mut at = 4;
    room(w, at, vendor.into(), "vorbis comment: vendor string")?;
    at += u64::from(vendor);
    let Some(count) = u32_le(w, at)? else {
        return Ok(Walk::Short);
    };
    at += 4;
    room(w, at, u64::from(count) * 4, "vorbis comment: list")?;
    for _ in 0..count {
        let Some(len) = u32_le(w, at)? else {
            return Ok(Walk::Short);
        };
        at += 4;
        room(w, at, len.into(), "vorbis comment")?;
        if let Some((value, value_len)) = picture_value(w, at, len.into())? {
            picture(&mut Base64::new(w, value, value_len)?)?;
        }
        at += u64::from(len);
    }
    Ok(Walk::Done)
}

/// Where the base64 value of the comment of `len` bytes at `at` lies, if it is a
/// `METADATA_BLOCK_PICTURE` comment: its key matched as the reader matches it, lossily decoded
/// and lowercased. No key longer than 96 bytes lowercases to that one.
fn picture_value(w: &mut dyn Window, at: u64, len: u64) -> io::Result<Option<(u64, u64)>> {
    let mut head = [0u8; 96];
    let n = len.min(96) as usize;
    if !w.read(at, &mut head[..n])? {
        return Ok(None);
    }
    let Some(eq) = head[..n].iter().position(|&b| b == b'=') else {
        return Ok(None);
    };
    if String::from_utf8_lossy(&head[..eq]).to_lowercase() != "metadata_block_picture" {
        return Ok(None);
    }
    let eq = eq as u64;
    Ok(Some((at + eq + 1, len - eq - 1)))
}

/// Walks a FLAC picture from the start of `w`: type, media type, description, four dimensions,
/// data. The reader reserves the media type, the description and the data from their lengths.
fn picture(w: &mut dyn Window) -> Result<Walk, Refusal> {
    let mut at = 4;
    for (what, skip_after) in [
        ("flac picture: media type", 0),
        ("flac picture: description", 16),
        ("flac picture: data", 0),
    ] {
        let Some(len) = u32_be(w, at)? else {
            return Ok(Walk::Short);
        };
        at += 4;
        room(w, at, len.into(), what)?;
        at += u64::from(len) + skip_after;
    }
    Ok(Walk::Done)
}

// ---------------------------------------------------------------------------------------------
// File access

/// Bytes the cache holds: enough for a run of headers, small enough that hopping from header to
/// header across a large file reads little more than the headers.
const CACHE: usize = 16 << 10;

/// Random access to the file through one small cache.
struct Source<R> {
    inner: R,
    len: u64,
    cache: Vec<u8>,
    cache_at: u64,
}

impl<R: Read + Seek> Source<R> {
    fn new(mut inner: R) -> io::Result<Self> {
        let len = inner.seek(SeekFrom::End(0))?;
        Ok(Source {
            inner,
            len,
            cache: Vec::new(),
            cache_at: 0,
        })
    }

    /// Copies what the file holds at `at` into the front of `out` and returns how much: less
    /// than asked only where the file ends.
    fn read_upto(&mut self, at: u64, out: &mut [u8]) -> io::Result<usize> {
        let n = self.len.saturating_sub(at).min(out.len() as u64) as usize;
        if n == 0 {
            return Ok(0);
        }
        let end = at + n as u64;
        if at < self.cache_at || end > self.cache_at + self.cache.len() as u64 {
            if n > CACHE {
                self.inner.seek(SeekFrom::Start(at))?;
                self.inner.read_exact(&mut out[..n])?;
                return Ok(n);
            }
            self.cache
                .resize((self.len - at).min(CACHE as u64) as usize, 0);
            self.inner.seek(SeekFrom::Start(at))?;
            self.inner.read_exact(&mut self.cache)?;
            self.cache_at = at;
        }
        let from = (at - self.cache_at) as usize;
        out[..n].copy_from_slice(&self.cache[from..from + n]);
        Ok(n)
    }

    /// Fills `out` from `at`; false when the file ends first.
    fn read_at(&mut self, at: u64, out: &mut [u8]) -> io::Result<bool> {
        Ok(self.read_upto(at, out)? == out.len())
    }

    fn u16_be(&mut self, at: u64) -> io::Result<Option<u16>> {
        let mut b = [0u8; 2];
        Ok(self.read_at(at, &mut b)?.then(|| u16::from_be_bytes(b)))
    }

    fn u32_be(&mut self, at: u64) -> io::Result<Option<u32>> {
        let mut b = [0u8; 4];
        Ok(self.read_at(at, &mut b)?.then(|| u32::from_be_bytes(b)))
    }

    fn u64_be(&mut self, at: u64) -> io::Result<Option<u64>> {
        let mut b = [0u8; 8];
        Ok(self.read_at(at, &mut b)?.then(|| u64::from_be_bytes(b)))
    }

    fn i64_be(&mut self, at: u64) -> io::Result<Option<i64>> {
        Ok(self.u64_be(at)?.map(|v| v as i64))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn screened(bytes: &[u8]) -> Result<(), String> {
        screen(&mut Cursor::new(bytes)).map_err(|e| e.to_string())
    }

    fn refused(bytes: &[u8]) -> bool {
        matches!(screen(&mut Cursor::new(bytes)), Err(Refusal::Impossible(_)))
    }

    fn atom(kind: &[u8; 4], payload: &[u8]) -> Vec<u8> {
        let mut a = ((payload.len() + 8) as u32).to_be_bytes().to_vec();
        a.extend_from_slice(kind);
        a.extend_from_slice(payload);
        a
    }

    /// A table atom: version and flags, `fields` ahead of the count, the count, then `entries`
    /// bytes of table.
    fn table(kind: &[u8; 4], fields: &[u32], count: u32, entries: usize) -> Vec<u8> {
        let mut p = vec![0u8; 4];
        for f in fields {
            p.extend_from_slice(&f.to_be_bytes());
        }
        p.extend_from_slice(&count.to_be_bytes());
        p.resize(p.len() + entries, 0);
        atom(kind, &p)
    }

    /// An ftyp, then a moov with one track whose stbl holds `stbl`, then `tail`.
    fn mp4(stbl: &[Vec<u8>], tail: &[u8]) -> Vec<u8> {
        let stbl = atom(b"stbl", &stbl.concat());
        let moov = atom(
            b"moov",
            &atom(b"trak", &atom(b"mdia", &atom(b"minf", &stbl))),
        );
        [atom(b"ftyp", b"M4A \0\0\0\0"), moov, tail.to_vec()].concat()
    }

    #[test]
    fn sample_tables_that_fit_pass() {
        let file = mp4(
            &[
                table(b"stts", &[], 2, 16),
                table(b"stsc", &[], 1, 12),
                table(b"stsz", &[0], 3, 12),
                table(b"stco", &[], 2, 8),
                table(b"co64", &[], 1, 8),
            ],
            &atom(b"mdat", &[0; 32]),
        );
        assert_eq!(screened(&file), Ok(()));
    }

    #[test]
    fn a_sample_table_counting_past_its_atom_is_refused() {
        for t in [
            table(b"stco", &[], 3, 8),
            table(b"co64", &[], 2, 8),
            table(b"stts", &[], 2, 8),
            table(b"stsc", &[], 2, 12),
            table(b"stsz", &[0], 4, 12),
            table(b"stco", &[], u32::MAX, 4),
        ] {
            assert!(
                refused(&mp4(std::slice::from_ref(&t), &[])),
                "{:?}",
                &t[4..8]
            );
        }
        // A constant sample size leaves no table behind the count.
        assert_eq!(
            screened(&mp4(&[table(b"stsz", &[1], u32::MAX, 0)], &[])),
            Ok(())
        );
        // Too short to hold its count, the reader would take one from the next atom's bytes.
        assert!(refused(&mp4(&[atom(b"stco", &[0; 4])], &[])));
    }

    #[test]
    fn an_atom_larger_than_its_parent_is_refused() {
        let mut inner = table(b"stco", &[], 0, 0);
        inner[0..4].copy_from_slice(&1000u32.to_be_bytes());
        assert!(refused(&mp4(&[inner], &[])));
        // The same through a 64-bit size.
        let mut big = vec![0, 0, 0, 1];
        big.extend_from_slice(b"stco");
        big.extend_from_slice(&u64::MAX.to_be_bytes());
        assert!(refused(&mp4(&[big], &[])));
    }

    #[test]
    fn a_truncated_mdat_at_the_end_still_passes() {
        let mut mdat = atom(b"mdat", &[0; 16]);
        mdat[0..4].copy_from_slice(&1_000_000u32.to_be_bytes());
        assert_eq!(screened(&mp4(&[table(b"stco", &[], 1, 4)], &mdat)), Ok(()));
    }

    #[test]
    fn an_itunes_tag_larger_than_its_item_is_refused() {
        let file = |data: Vec<u8>| {
            let ilst = atom(b"ilst", &atom(b"\xa9too", &data));
            let meta = atom(b"meta", &[vec![0; 4], ilst].concat());
            let moov = atom(b"moov", &atom(b"udta", &meta));
            [atom(b"ftyp", b"M4A \0\0\0\0"), moov].concat()
        };
        let good = atom(b"data", &[0, 0, 0, 1, 0, 0, 0, 0, b'x']);
        let mut bad = good.clone();
        bad[0..4].copy_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(screened(&file(good)), Ok(()));
        assert!(refused(&file(bad)));
    }

    fn caf(chunks: &[(&[u8; 4], i64, Vec<u8>)]) -> Vec<u8> {
        let mut f = b"caff\0\x01\0\0".to_vec();
        for (kind, size, body) in chunks {
            f.extend_from_slice(*kind);
            f.extend_from_slice(&size.to_be_bytes());
            f.extend_from_slice(body);
        }
        f
    }

    /// An AAC description: variable bytes per packet, 1024 frames each.
    fn desc() -> (&'static [u8; 4], i64, Vec<u8>) {
        let mut d = 44_100f64.to_be_bytes().to_vec();
        d.extend_from_slice(b"aac ");
        for v in [2u32, 0, 1024, 1, 0] {
            d.extend_from_slice(&v.to_be_bytes());
        }
        (b"desc", 32, d)
    }

    fn pakt(packets: i64, entries: &[u8]) -> (&'static [u8; 4], i64, Vec<u8>) {
        let mut p = packets.to_be_bytes().to_vec();
        p.extend_from_slice(&[0; 16]);
        p.extend_from_slice(entries);
        (b"pakt", p.len() as i64, p)
    }

    #[test]
    fn a_caf_packet_table_that_fits_passes() {
        let data = (b"data", 8, vec![0; 8]);
        assert_eq!(screened(&caf(&[desc(), pakt(2, &[2, 2]), data])), Ok(()));
    }

    #[test]
    fn a_caf_packet_count_past_its_chunk_is_refused() {
        assert!(refused(&caf(&[desc(), pakt(1 << 32, &[2, 2])])));
        assert!(refused(&caf(&[desc(), pakt(3, &[2, 2])])));
    }

    #[test]
    fn a_caf_cookie_past_the_file_is_refused() {
        assert!(refused(&caf(&[desc(), (b"kuki", 1 << 40, vec![0; 4])])));
        assert_eq!(screened(&caf(&[desc(), (b"kuki", 4, vec![0; 4])])), Ok(()));
    }

    fn flac(blocks: &[(u8, Vec<u8>)]) -> Vec<u8> {
        let mut all = vec![(0u8, vec![0u8; 34])];
        all.extend_from_slice(blocks);
        let mut f = b"fLaC".to_vec();
        for (i, (kind, body)) in all.iter().enumerate() {
            let last = if i + 1 == all.len() { 0x80 } else { 0 };
            f.push(kind | last);
            f.extend_from_slice(&(body.len() as u32).to_be_bytes()[1..]);
            f.extend_from_slice(body);
        }
        f
    }

    fn picture(mime_len: u32, desc_len: u32, data_len: u32) -> Vec<u8> {
        let mut p = 3u32.to_be_bytes().to_vec();
        p.extend_from_slice(&mime_len.to_be_bytes());
        p.extend_from_slice(b"image/png");
        p.extend_from_slice(&desc_len.to_be_bytes());
        p.extend_from_slice(&[0; 16]);
        p.extend_from_slice(&data_len.to_be_bytes());
        p.extend_from_slice(&[0; 4]);
        p
    }

    /// A Vorbis comment list of `list`, the first comment's length given as `first_len` when set.
    fn comments(list: &[&[u8]], first_len: Option<u32>) -> Vec<u8> {
        let mut c = 3u32.to_le_bytes().to_vec();
        c.extend_from_slice(b"lib");
        c.extend_from_slice(&(list.len() as u32).to_le_bytes());
        for (i, s) in list.iter().enumerate() {
            let len = first_len.filter(|_| i == 0).unwrap_or(s.len() as u32);
            c.extend_from_slice(&len.to_le_bytes());
            c.extend_from_slice(s);
        }
        c
    }

    #[test]
    fn flac_pictures_and_comments_that_fit_pass() {
        let file = flac(&[(4, comments(&[b"TITLE=tone"], None)), (6, picture(9, 0, 4))]);
        assert_eq!(screened(&file), Ok(()));
    }

    #[test]
    fn a_flac_picture_length_past_its_block_is_refused() {
        assert!(refused(&flac(&[(6, picture(u32::MAX, 0, 4))])));
        assert!(refused(&flac(&[(6, picture(9, u32::MAX, 4))])));
        assert!(refused(&flac(&[(6, picture(9, 0, u32::MAX))])));
    }

    #[test]
    fn a_vorbis_comment_length_past_its_block_is_refused() {
        let block = comments(&[b"TITLE=tone"], Some(u32::MAX));
        assert!(refused(&flac(&[(4, block)])));
    }

    fn base64(bytes: &[u8]) -> Vec<u8> {
        const ALPHABET: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut s = Vec::new();
        for c in bytes.chunks(3) {
            let n = (u32::from(c[0]) << 16)
                | (u32::from(*c.get(1).unwrap_or(&0)) << 8)
                | u32::from(*c.get(2).unwrap_or(&0));
            for i in 0..=c.len() {
                s.push(ALPHABET[(n >> (18 - 6 * i) & 63) as usize]);
            }
        }
        while s.len() % 4 != 0 {
            s.push(b'=');
        }
        s
    }

    #[test]
    fn a_picture_carried_in_a_comment_is_checked_too() {
        let file = |p: Vec<u8>| {
            let comment = [b"METADATA_BLOCK_PICTURE=".as_slice(), &base64(&p)].concat();
            flac(&[(4, comments(&[&comment], None))])
        };
        assert_eq!(screened(&file(picture(9, 0, 4))), Ok(()));
        assert!(refused(&file(picture(u32::MAX, 0, 4))));
    }

    /// An Ogg page on stream 1 completing each of `packets`.
    fn page(sequence: u32, first: bool, packets: &[&[u8]]) -> Vec<u8> {
        let mut segments = Vec::new();
        for p in packets {
            segments.extend(std::iter::repeat_n(255u8, p.len() / 255));
            segments.push((p.len() % 255) as u8);
        }
        let mut page = b"OggS\0".to_vec();
        page.push(if first { 2 } else { 0 });
        page.extend_from_slice(&0u64.to_le_bytes());
        page.extend_from_slice(&1u32.to_le_bytes());
        page.extend_from_slice(&sequence.to_le_bytes());
        page.extend_from_slice(&[0; 4]);
        page.push(segments.len() as u8);
        page.extend_from_slice(&segments);
        for p in packets {
            page.extend_from_slice(p);
        }
        let sum = ogg_crc(0, &page);
        page[22..26].copy_from_slice(&sum.to_le_bytes());
        page
    }

    fn vorbis_ogg(comment_packet: &[u8]) -> Vec<u8> {
        let mut ident = b"\x01vorbis".to_vec();
        ident.resize(30, 0);
        [
            page(0, true, &[&ident]),
            page(1, false, &[comment_packet]),
            page(2, false, &[&[0u8; 4]]),
        ]
        .concat()
    }

    fn vorbis_comment_packet(first_len: Option<u32>) -> Vec<u8> {
        [
            b"\x03vorbis".as_slice(),
            &comments(&[b"TITLE=tone"], first_len),
        ]
        .concat()
    }

    #[test]
    fn ogg_vorbis_comments_are_checked() {
        assert_eq!(screened(&vorbis_ogg(&vorbis_comment_packet(None))), Ok(()));
        assert!(refused(&vorbis_ogg(&vorbis_comment_packet(Some(u32::MAX)))));
    }

    #[test]
    fn a_page_failing_its_checksum_is_skipped_as_the_reader_skips_it() {
        let mut file = vorbis_ogg(&vorbis_comment_packet(Some(u32::MAX)));
        // The comment page starts after the 58-byte identification page.
        file[58 + 22] ^= 1;
        assert_eq!(screened(&file), Ok(()));
    }

    #[test]
    fn the_container_is_found_where_the_probe_finds_it() {
        let bad = flac(&[(6, picture(u32::MAX, 0, 4))]);
        // Junk ahead: the probe still finds the FLAC marker behind it.
        assert!(refused(&[b"junk".as_slice(), &bad].concat()));
        // An ID3v2 tag ahead: the probe reads past it.
        let id3 = b"ID3\x04\0\0\0\0\0\x04\0\0\0\0";
        assert!(refused(&[id3.as_slice(), &bad].concat()));
        // An MPEG frame sync ahead: the probe takes the file as MP3, read without FLAC blocks.
        assert_eq!(
            screened(&[b"\xff\xfb\0\0".as_slice(), &bad].concat()),
            Ok(())
        );
    }

    #[test]
    fn the_engine_fixtures_pass() {
        let dir = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures");
        for entry in std::fs::read_dir(dir).unwrap() {
            let path = entry.unwrap().path();
            let mut file = std::fs::File::open(&path).unwrap();
            assert!(screen(&mut file).is_ok(), "{}", path.display());
        }
    }

    #[test]
    fn the_source_is_left_where_it_was() {
        let mut cursor = Cursor::new(mp4(&[table(b"stco", &[], 1, 4)], &[]));
        cursor.set_position(5);
        screen(&mut cursor).unwrap();
        assert_eq!(cursor.position(), 5);
    }
}
