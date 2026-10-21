//! Answers: `ESC _ G i=<id>[,I=<n>][,p=<p>];OK ESC \` or `…;<CODE>:<message> ESC \`.

use super::Error;
use super::command::Command;

pub fn ok(cmd: &Command, out: &mut Vec<u8>) {
    if cmd.quiet == 0 {
        write(cmd, "OK", out);
    }
}

pub fn error(cmd: &Command, e: &Error, out: &mut Vec<u8>) {
    if cmd.quiet < 2 {
        write(cmd, &format!("{}:{}", e.code, e.msg), out);
    }
}

fn write(cmd: &Command, msg: &str, out: &mut Vec<u8>) {
    if !cmd.answer {
        return;
    }
    let mut keys = Vec::new();
    if cmd.id != 0 {
        keys.push(format!("i={}", cmd.id));
    }
    if cmd.number != 0 {
        keys.push(format!("I={}", cmd.number));
    }
    if cmd.placement != 0 {
        keys.push(format!("p={}", cmd.placement));
    }
    out.extend_from_slice(b"\x1b_G");
    out.extend_from_slice(keys.join(",").as_bytes());
    out.push(b';');
    out.extend(msg.chars().filter(|c| !c.is_control()).collect::<String>().bytes());
    out.extend_from_slice(b"\x1b\\");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::graphics::command::parse;

    fn reply(control: &str, err: Option<Error>) -> Vec<u8> {
        let cmd = parse(control.as_bytes()).0;
        let mut out = vec![];
        match err {
            Some(e) => error(&cmd, &e, &mut out),
            None => ok(&cmd, &mut out),
        }
        out
    }

    #[test]
    fn ok_and_quiet_levels() {
        assert_eq!(reply("i=31", None), b"\x1b_Gi=31;OK\x1b\\");
        assert_eq!(reply("i=31,I=2,p=5", None), b"\x1b_Gi=31,I=2,p=5;OK\x1b\\");
        assert!(reply("i=31,q=1", None).is_empty());
        assert!(reply("f=24", None).is_empty(), "no id, no reply");
        let e = || Some(Error::new("ENOENT", "gone\x1b]x"));
        assert_eq!(reply("i=3,q=1", e()), b"\x1b_Gi=3;ENOENT:gone]x\x1b\\");
        assert!(reply("i=3,q=2", e()).is_empty());
    }
}
