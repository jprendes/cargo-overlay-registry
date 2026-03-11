use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug)]
struct Message {
    greeting: String,
    from: String,
}

fn main() {
    let msg = Message {
        greeting: "Hello, World!".to_string(),
        from: "Cargo Proxy Registry".to_string(),
    };

    let json = serde_json::to_string_pretty(&msg).unwrap();
    println!("{}", json);
}
