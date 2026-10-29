fn main() -> std::io::Result<()> {
    std::fs::write("agent-result.txt", "Synthetic agent change\n")?;
    println!("DEMO_API_KEY=violet-otter");
    println!("PRIVATE-LAB-123456");
    println!("the weather is pleasant");
    Ok(())
}
