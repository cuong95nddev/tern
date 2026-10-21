//! A command's control data: `key=value` pairs separated by commas.

use super::Error;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Transmit,
    TransmitAndPlace,
    Query,
    Place,
    Delete,
    Other(u8),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Rgb,
    Rgba,
    Png,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Medium {
    Direct,
    File,
    TempFile,
    SharedMemory,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Command {
    pub action: Action,
    /// 1: no OK replies; 2: no replies at all.
    pub quiet: u8,
    pub id: u32,
    pub number: u32,
    pub placement: u32,
    pub format: Format,
    pub medium: Medium,
    pub width: u32,
    pub height: u32,
    pub size: u64,
    pub offset: u64,
    pub more: bool,
    pub compressed: bool,
    pub unicode: bool,
    pub cols: u32,
    pub rows: u32,
    pub delete: u8,
    /// The program named the image (`i` or `I`), so it wants replies.
    pub answer: bool,
}

impl Default for Command {
    fn default() -> Self {
        Self {
            action: Action::Transmit,
            quiet: 0,
            id: 0,
            number: 0,
            placement: 0,
            format: Format::Rgba,
            medium: Medium::Direct,
            width: 0,
            height: 0,
            size: 0,
            offset: 0,
            more: false,
            compressed: false,
            unicode: false,
            cols: 0,
            rows: 0,
            delete: 0,
            answer: false,
        }
    }
}

/// Parses control data. A bad value doesn't stop parsing, so the reply can
/// still name the image; the first error is returned beside the command.
pub fn parse(control: &[u8]) -> (Command, Option<Error>) {
    let mut cmd = Command::default();
    let mut err = None;
    for pair in control.split(|&b| b == b',').filter(|p| !p.is_empty()) {
        let Some(eq) = pair.iter().position(|&b| b == b'=') else {
            err.get_or_insert(Error::new("EINVAL", "malformed control data"));
            continue;
        };
        if let [key] = pair[..eq]
            && let Err(e) = apply(&mut cmd, key, &pair[eq + 1..])
        {
            err.get_or_insert(e);
        }
    }
    cmd.answer = cmd.id != 0 || cmd.number != 0;
    (cmd, err)
}

fn apply(cmd: &mut Command, key: u8, value: &[u8]) -> Result<(), Error> {
    let bad = || Error::new("EINVAL", format!("bad value for {}", key as char));
    let ch = || match value {
        [c] => Ok(*c),
        _ => Err(bad()),
    };
    let num = || std::str::from_utf8(value).ok().and_then(|s| s.parse::<u64>().ok()).ok_or_else(bad);
    let num32 = || num().and_then(|n| u32::try_from(n).map_err(|_| bad()));
    match key {
        b'a' => {
            cmd.action = match ch()? {
                b't' => Action::Transmit,
                b'T' => Action::TransmitAndPlace,
                b'q' => Action::Query,
                b'p' => Action::Place,
                b'd' => Action::Delete,
                c => Action::Other(c),
            }
        },
        b'q' => cmd.quiet = num32()?.min(2) as u8,
        b'i' => cmd.id = num32()?,
        b'I' => cmd.number = num32()?,
        b'p' => cmd.placement = num32()?,
        b'f' => {
            cmd.format = match num32()? {
                24 => Format::Rgb,
                32 => Format::Rgba,
                100 => Format::Png,
                _ => return Err(bad()),
            }
        },
        b't' => {
            cmd.medium = match ch()? {
                b'd' => Medium::Direct,
                b'f' => Medium::File,
                b't' => Medium::TempFile,
                b's' => Medium::SharedMemory,
                _ => return Err(bad()),
            }
        },
        b's' => cmd.width = num32()?,
        b'v' => cmd.height = num32()?,
        b'S' => cmd.size = num()?,
        b'O' => cmd.offset = num()?,
        b'm' => cmd.more = num()? == 1,
        b'o' => cmd.compressed = ch()? == b'z',
        b'U' => cmd.unicode = num()? == 1,
        b'c' => cmd.cols = num32()?,
        b'r' => cmd.rows = num32()?,
        b'd' => cmd.delete = ch()?,
        _ => {},
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults() {
        let (c, e) = parse(b"");
        assert!(e.is_none());
        assert_eq!((c.action, c.format, c.medium, c.answer), (Action::Transmit, Format::Rgba, Medium::Direct, false));
    }

    #[test]
    fn every_key() {
        let (c, e) = parse(b"a=T,q=2,i=31,I=4,p=7,f=100,t=s,s=3,v=2,S=10,O=5,m=1,U=1,c=40,r=12,d=I,X=9,zz=1");
        assert!(e.is_none());
        assert_eq!(
            c,
            Command {
                action: Action::TransmitAndPlace,
                quiet: 2,
                id: 31,
                number: 4,
                placement: 7,
                format: Format::Png,
                medium: Medium::SharedMemory,
                width: 3,
                height: 2,
                size: 10,
                offset: 5,
                more: true,
                compressed: false,
                unicode: true,
                cols: 40,
                rows: 12,
                delete: b'I',
                answer: true,
            }
        );
    }

    #[test]
    fn bad_value_keeps_the_ids() {
        let (c, e) = parse(b"i=9,f=7");
        assert_eq!(c.id, 9);
        assert_eq!(e.unwrap().code, "EINVAL");
        assert_eq!(parse(b"i=x").1.unwrap().code, "EINVAL");
    }

    #[test]
    fn actions() {
        assert_eq!(parse(b"a=q").0.action, Action::Query);
        assert_eq!(parse(b"a=p").0.action, Action::Place);
        assert_eq!(parse(b"a=d").0.action, Action::Delete);
        assert_eq!(parse(b"a=f").0.action, Action::Other(b'f'));
    }
}
