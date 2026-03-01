use schemars::schema_for;

fn main() {
    let schema = schema_for!(agent_box_common::config::Config);
    println!("{}", serde_json::to_string_pretty(&schema).unwrap());
}
