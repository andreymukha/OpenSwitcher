use std::process::Command;

fn is_russian_layout() -> bool {
    let output = Command::new("xset")
        .env("DISPLAY", ":0.0")
        .arg("-q")
        .output();
        
    if let Ok(out) = output {
        let text = String::from_utf8_lossy(&out.stdout);
        for line in text.lines() {
            if line.contains("LED mask:") {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if let Some(mask_str) = parts.last() {
                    if let Ok(mask) = u32::from_str_radix(mask_str, 16) {
                        return (mask & 0x1000) != 0;
                    }
                }
            }
        }
    }
    false
}

fn main() {
    println!("Testing layout detection...");
    for _ in 0..5 {
        println!("Russian layout active? {}", is_russian_layout());
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}
