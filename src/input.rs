use sysinfo::Pid;

#[derive(Clone, Debug)]
pub enum Target {
    Pid(Pid),
    Port(u16),
    Name(String),
}

pub fn parse_targets(input: &[String]) -> Vec<Target> {
    input.iter().map(|item| parse_target(item)).collect()
}

fn parse_target(input: &str) -> Target {
    if let Some(port) = input
        .strip_prefix(':')
        .and_then(|value| value.parse::<u16>().ok())
    {
        return Target::Port(port);
    }

    if let Ok(pid) = input.parse::<u32>() {
        return Target::Pid(Pid::from_u32(pid));
    }

    Target::Name(input.to_string())
}

#[cfg(test)]
mod tests {
    use sysinfo::Pid;

    use super::{Target, parse_targets};

    #[test]
    fn parses_mixed_targets() {
        let parsed = parse_targets(&["1234".to_string(), ":3000".to_string(), "node".to_string()]);

        assert!(matches!(parsed[0], Target::Pid(pid) if pid == Pid::from_u32(1234)));
        assert!(matches!(parsed[1], Target::Port(3000)));
        assert!(matches!(parsed[2], Target::Name(ref name) if name == "node"));
    }

    #[test]
    fn keeps_invalid_port_like_input_as_name() {
        let parsed = parse_targets(&[":abc".to_string()]);
        assert!(matches!(parsed[0], Target::Name(ref name) if name == ":abc"));
    }
}
