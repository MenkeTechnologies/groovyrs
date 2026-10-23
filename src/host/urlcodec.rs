//! `java.net.URLEncoder.encode` / `URLDecoder.decode`, ported from JDK 21:
//! `application/x-www-form-urlencoded` over a named charset. UTF-8 (the
//! default the one-argument forms use) and ISO-8859-1 are modeled.

/// A charset the codec can encode and decode through.
#[derive(Clone, Copy)]
pub(super) enum Charset {
    Utf8,
    Latin1,
}

/// `Charset.forName`, for the charsets modeled: their canonical names and
/// the aliases the JDK registers for them, case-insensitively. `None` is the
/// `UnsupportedEncodingException` the `String`-named overloads raise.
pub(super) fn charset(name: &str) -> Option<Charset> {
    match name.to_ascii_uppercase().as_str() {
        "UTF-8" | "UTF8" | "UNICODE-1-1-UTF-8" => Some(Charset::Utf8),
        "ISO-8859-1" | "ISO8859-1" | "ISO8859_1" | "ISO_8859-1" | "ISO_8859_1" | "LATIN1"
        | "L1" | "8859_1" | "CP819" | "IBM819" | "819" | "ISO-IR-100" | "CSISOLATIN1" => {
            Some(Charset::Latin1)
        }
        _ => None,
    }
}

/// The bytes `text` encodes to; an unmappable Latin-1 character is the
/// encoder's replacement `?`.
fn encode_bytes(text: &str, cs: Charset) -> Vec<u8> {
    match cs {
        Charset::Utf8 => text.as_bytes().to_vec(),
        Charset::Latin1 => text
            .chars()
            .map(|c| u8::try_from(u32::from(c)).unwrap_or(b'?'))
            .collect(),
    }
}

/// `URLEncoder.encode(s, charset)`: letters, digits and `.-*_` pass through,
/// a space becomes `+`, and every other run of characters is encoded and
/// written as `%XX` per byte, upper-case hex.
pub(super) fn encode(s: &str, cs: Charset) -> String {
    let keep = |c: char| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '*' | '_');
    let mut out = String::with_capacity(s.len());
    let mut run = String::new();
    let flush = |run: &mut String, out: &mut String| {
        for b in encode_bytes(run, cs) {
            out.push_str(&format!("%{b:02X}"));
        }
        run.clear();
    };
    for c in s.chars() {
        if keep(c) || c == ' ' {
            flush(&mut run, &mut out);
            out.push(if c == ' ' { '+' } else { c });
        } else {
            run.push(c);
        }
    }
    flush(&mut run, &mut out);
    out
}

/// `URLDecoder.decode(s, charset)`: `+` is a space, and each run of `%XX`
/// escapes is gathered into bytes and decoded together (a malformed sequence
/// decodes to U+FFFD). `Err` carries the `IllegalArgumentException` message.
pub(super) fn decode(s: &str, cs: Charset) -> Result<String, String> {
    let chars: Vec<char> = s.chars().collect();
    let n = chars.len();
    let mut out = String::with_capacity(s.len());
    let mut i = 0;
    while i < n {
        match chars[i] {
            '+' => {
                out.push(' ');
                i += 1;
            }
            '%' => {
                let mut bytes = Vec::new();
                while i + 2 < n && chars[i] == '%' {
                    let v = parse_hex_pair(&chars[i + 1..i + 3]).map_err(|e| {
                        format!("URLDecoder: Illegal hex characters in escape (%) pattern - {e}")
                    })?;
                    if v < 0 {
                        return Err(
                            "URLDecoder: Illegal hex characters in escape (%) pattern - \
                                    negative value"
                                .to_string(),
                        );
                    }
                    bytes.push(v as u8);
                    i += 3;
                }
                if i < n && chars[i] == '%' {
                    return Err("URLDecoder: Incomplete trailing escape (%) pattern".to_string());
                }
                match cs {
                    Charset::Utf8 => out.push_str(&String::from_utf8_lossy(&bytes)),
                    Charset::Latin1 => out.extend(bytes.iter().map(|b| char::from(*b))),
                }
            }
            c => {
                out.push(c);
                i += 1;
            }
        }
    }
    Ok(out)
}

/// `Integer.parseInt(s, i, i + 2, 16)` over the two characters after a `%`,
/// with its `NumberFormatException` message: an optional sign, then hex
/// digits (`Character.digit`, so a full-width digit counts).
fn parse_hex_pair(pair: &[char]) -> Result<i32, String> {
    let text: String = pair.iter().collect();
    let error = |at: usize| format!("Error at index {at} in: \"{text}\"");
    let (negative, digits_from) = match pair[0] {
        '-' => (true, 1),
        '+' => (false, 1),
        c if c < '0' => return Err(error(0)),
        _ => (false, 0),
    };
    let mut value = 0i32;
    for (at, c) in pair.iter().enumerate().skip(digits_from) {
        let digit = java_hex_digit(*c).ok_or_else(|| error(at))?;
        value = value * 16 + digit;
    }
    Ok(if negative { -value } else { value })
}

/// `Character.digit(c, 16)` for the ASCII and full-width hex digits and
/// letters (the other Unicode decimal digits are not modeled).
fn java_hex_digit(c: char) -> Option<i32> {
    if let Some(d) = c.to_digit(16) {
        return Some(d as i32);
    }
    match c {
        '\u{FF10}'..='\u{FF19}' => Some(c as i32 - 0xFF10),
        '\u{FF21}'..='\u{FF26}' => Some(c as i32 - 0xFF21 + 10),
        '\u{FF41}'..='\u{FF46}' => Some(c as i32 - 0xFF41 + 10),
        _ => None,
    }
}
