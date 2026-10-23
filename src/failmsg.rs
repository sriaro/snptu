
const FORMATS: &[&str] = &["HEIC", "AVIF", "JPEG XL", "JPEG 2000", "Photoshop", "SVG", "RAW", "WebP", "GIF"];

const OURS: &[&str] = &[
    "pictures coded as ",
    "transparency ",
    "the decoder returned ",
    "only rgb ",
    "only 8-bit ",
    "large documents ",
    "the document ",
    "the file is cut off",
    "the file is damaged",
    "a row of the composite ",
    "the composite uses ",
    "can't allocate ",
    "the frames differ ",
    "frame size differs ",
    "a frame differs ",
    "a picture is far larger ",
];

const TAGGED: &[(&str, &str, &str)] = &[
    ("", "unsupported pixel layout", "This picture's pixel layout isn't supported"),
    ("", "image too large", "The picture is too big to open"),
    ("WebP", "invalid dimensions", "The picture is too big for WebP. Try a smaller size"),
    ("WebP", "partition 0 overflow", "The picture is too detailed for a lossy WebP. Try lossless, or a smaller size"),
];

#[derive(Clone, Copy, Debug)]
enum Where {
    Has,
    Starts,
    Ends,
}

impl Where {
    fn hits(self, low: &str, phrase: &str) -> bool {
        match self {
            Where::Has => low.contains(phrase),
            Where::Starts => low.starts_with(phrase),
            Where::Ends => low.ends_with(phrase),
        }
    }
}

const KNOWN: &[(Where, &str, &str)] = &[
    (Where::Has, "it is damaged, too large, or not supported", "Snptu couldn't read this file. It may be damaged, too big, or in a format Snptu doesn't support"),
    (Where::Ends, "is not a picture format snptu reads", "This isn't a picture format Snptu can open"),
    (Where::Starts, "no free file name", "Snptu couldn't find a free name for the result in that folder"),
    (Where::Has, "the image is empty", "The picture is empty"),
    (Where::Has, "the animation is too large", "This animation is too big to convert. It has too many frames, or they are too large"),
    (Where::Has, "the file is too large to open", "The file is too big to open"),
    (Where::Has, "too large to open", "The picture is too big to open"),
    (Where::Has, "size exceeds limit", "The picture is too big to open"),
    (Where::Has, "colour profile is too large", "The picture's colour profile is too big"),
    (Where::Has, "colour profile:", "The picture's colour profile is damaged or not supported"),
    (Where::Has, "webp can't be larger", "A WebP can't be larger than 16384 px on a side (16383 when lossy). Try a smaller size"),
    (Where::Has, "gif can't be larger", "A GIF can't be larger than 65535 px on a side. Try a smaller size"),
    (Where::Has, "the result would be larger than", "The result would be too big (over 2 GB). Try a smaller size or fewer frames"),
    (Where::Has, "is too large", "The converted picture would be too big. Try a smaller size"),
    (Where::Has, "can't hold more than 8 bits", "This format can't hold more than 8 bits per colour"),
    (Where::Has, "only gif and webp can hold an animation", "Only GIF and WebP can hold an animation"),
    (Where::Has, "the animation has no frames", "The animation has no frames"),
    (Where::Has, "can't start the decoder", "Snptu couldn't get an answer from its file reader. Try again"),
    (Where::Has, "can't find snptu.exe", "Snptu couldn't get an answer from its file reader. Try again"),
    (Where::Has, "no decoder output", "Snptu couldn't get an answer from its file reader. Try again"),
    (Where::Has, "unexpected decoder answer", "Snptu couldn't get an answer from its file reader. Try again"),
    (Where::Ends, "decoding failed (internal error)", "Snptu couldn't decode this file. It may be damaged"),
    (Where::Ends, "(internal error)", "Something went wrong inside Snptu while converting this file"),
    (Where::Has, "started twice", "Something went wrong inside Snptu while converting this file"),
    (Where::Has, "rgba image", "Something went wrong inside Snptu while converting this file"),
    (Where::Has, "frames don't match", "The animation is damaged: its frames don't match its size"),
    (Where::Has, "failed to create whole tree", "Snptu can't create the folder for the results. Check that its drive or network location is connected"),
];

const LONGEST: usize = 160;

pub fn plain(raw: &str) -> String {
    let (reading, inner) = split_source(raw.trim());
    if let Some((format, body)) = inner.split_once(": ").filter(|(f, _)| FORMATS.contains(f)) {
        return tagged(format, body);
    }
    let low = inner.to_lowercase();
    if let Some((_, _, text)) = KNOWN.iter().find(|(at, phrase, _)| at.hits(&low, phrase)) {
        return text.to_string();
    }
    if let Some(code) = os_error(inner) {
        if let Some(text) = windows_error(code, reading) {
            return text.to_string();
        }
        if reading {
            let said = tidy(&strip_os_error(inner));
            return if said.is_empty() { "Windows couldn't read the file".to_string() } else { format!("Windows couldn't read the file: {said}") };
        }
    }
    if low.contains("resize:") {
        return "Snptu couldn't resize the picture".to_string();
    }
    if reading {
        return if low.contains("support") {
            "This picture uses something Snptu can't read yet".to_string()
        } else {
            "Snptu couldn't read the file. It may be damaged, or in a form Snptu can't read yet".to_string()
        };
    }
    let text = tidy(&strip_paths(&strip_os_error(inner)));
    if text.is_empty() { "The conversion failed".to_string() } else { capped(text) }
}

fn tagged(format: &str, body: &str) -> String {
    let low = body.to_lowercase();
    if let Some((_, _, text)) = TAGGED.iter().find(|(f, phrase, _)| (f.is_empty() || f == &format) && low.contains(phrase)) {
        return text.to_string();
    }
    if let Some(code) = os_error(body) {
        if let Some(text) = windows_error(code, false) {
            return text.to_string();
        }
        let said = tidy(&strip_paths(&strip_os_error(body)));
        return if said.is_empty() { "A file couldn't be read or written".to_string() } else { capped(said) };
    }
    let numbered_frame = low.strip_prefix("frame ").is_some_and(|rest| rest.starts_with(|c: char| c.is_ascii_digit()));
    if numbered_frame || OURS.iter().any(|o| low.starts_with(o)) {
        return capped(tidy(&format!("{format}: {}", strip_paths(body))));
    }
    format!("The {format} step failed. The picture may be damaged, too big, or not supported")
}

fn split_source(raw: &str) -> (bool, &str) {
    let reads = raw.get(..11).is_some_and(|p| p.eq_ignore_ascii_case("can't read ") || p.eq_ignore_ascii_case("can't open "));
    if !reads {
        return (false, raw);
    }
    match raw.find(": ") {
        Some(at) => (true, raw[at + 2..].trim()),
        None => (true, ""),
    }
}

fn os_error(raw: &str) -> Option<u32> {
    let at = raw.find("(os error ")? + "(os error ".len();
    let digits: String = raw[at..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

fn windows_error(code: u32, reading: bool) -> Option<&'static str> {
    Some(match (code, reading) {
        (2 | 3, true) => "The file wasn't found. It may have been moved or deleted",
        (2 | 3, false) => "A file or folder wasn't found. It may have been moved or deleted",
        (5, true) => "Windows denied access to the file",
        (5, false) => "Windows denied access to a file or folder",
        (32 | 33, true) => "Another program is using the file",
        (32 | 33, false) => "Another program is using a file this needs",
        (39 | 112, _) => "The disk is full",
        (19, _) => "The disk is write-protected",
        (21, _) => "The drive isn't ready",
        (53 | 64 | 67, _) => "The network location can't be reached",
        (123 | 161 | 206, true) => "The file's name or path isn't allowed",
        (123 | 161 | 206, false) => "A file name or folder path is too long or not allowed",
        _ => return None,
    })
}

fn strip_os_error(raw: &str) -> String {
    let Some(at) = raw.find("(os error ") else { return raw.to_string() };
    let before = raw[..at].trim_end();
    match raw[at..].find(')') {
        Some(end) => format!("{before}{}", &raw[at + end + 1..]),
        None => before.to_string(),
    }
}

fn strip_paths(s: &str) -> String {
    let c: Vec<char> = s.chars().collect();
    let mut out = String::new();
    let mut i = 0;
    while i < c.len() {
        let drive = i + 2 < c.len() && c[i].is_ascii_alphabetic() && c[i + 1] == ':' && matches!(c[i + 2], '\\' | '/') && (i == 0 || !c[i - 1].is_alphanumeric());
        let unc = i + 1 < c.len() && c[i] == '\\' && c[i + 1] == '\\';
        if drive || unc {
            out.push_str("the file");
            i += 2;
            while i < c.len() && !(c[i] == ':' && c.get(i + 1) == Some(&' ')) {
                i += 1;
            }
        } else {
            out.push(c[i]);
            i += 1;
        }
    }
    out
}

fn tidy(s: &str) -> String {
    let line = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let line = line.trim_end_matches(['.', '。']);
    let mut chars = line.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn capped(s: String) -> String {
    if s.chars().count() <= LONGEST { s } else { s.chars().take(LONGEST - 1).chain(std::iter::once('…')).collect() }
}
