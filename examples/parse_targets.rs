use fuckport::input::{Target, parse_targets};

fn main() {
    let sample = vec!["1234".to_string(), ":5173".to_string(), "node".to_string()];

    for target in parse_targets(&sample) {
        match target {
            Target::Pid(pid) => println!("pid {}", pid.as_u32()),
            Target::Port(port) => println!("port {port}"),
            Target::Name(name) => println!("name {name}"),
        }
    }
}
