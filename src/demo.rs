use anyhow::{Context, Result};
use rand::Rng;
use std::fs;
use std::io::Write;
use std::path::Path;

pub fn generate_demo_data(base_path: &Path, num_files: usize, duplicates_per_file: usize) -> Result<()> {
    // Remove existing demo directory if it exists
    if base_path.exists() {
        println!("Removing existing demo directory...");
        fs::remove_dir_all(base_path)?;
    }

    // Create base directory structure
    fs::create_dir_all(base_path)?;
    fs::create_dir_all(base_path.join("documents"))?;
    fs::create_dir_all(base_path.join("pictures"))?;
    fs::create_dir_all(base_path.join("downloads"))?;
    fs::create_dir_all(base_path.join("temp"))?;
    fs::create_dir_all(base_path.join("backup/old"))?;
    fs::create_dir_all(base_path.join("projects/project1/src"))?;
    fs::create_dir_all(base_path.join("projects/project2/src"))?;

    println!("Creating demo test data in: {}", base_path.display());
    println!("  {} unique files with {} duplicates each", num_files, duplicates_per_file);

    let mut rng = rand::thread_rng();
    let mut total_files = 0;
    let mut total_duplicates = 0;

    // Generate unique content patterns
    let content_templates = vec![
        "Important document content",
        "Project source code",
        "Configuration settings",
        "Data analysis results",
        "Meeting notes",
        "Photo metadata",
        "Build artifacts",
        "Log file entries",
    ];

    for i in 0..num_files {
        // Generate unique content for this file
        let template = content_templates[i % content_templates.len()];
        let size = rng.gen_range(100..10000);
        let content = generate_content(template, size, i);

        // Original file location
        let original_dir = match i % 4 {
            0 => "documents",
            1 => "pictures",
            2 => "projects/project1/src",
            _ => "projects/project2/src",
        };

        let filename = format!("file_{}.txt", i);
        let original_path = base_path.join(original_dir).join(&filename);

        fs::write(&original_path, &content)
            .context(format!("Failed to write {}", original_path.display()))?;
        total_files += 1;

        // Create duplicates in various locations with different naming patterns
        for j in 0..duplicates_per_file {
            let (dup_dir, dup_name) = match j % 6 {
                0 => ("downloads", format!("file_{}.txt", i)),
                1 => ("temp", format!("file_{}_temp.txt", i)),
                2 => ("backup", format!("file_{}_copy.txt", i)),
                3 => ("backup/old", format!("file_{} (1).txt", i)),
                4 => (original_dir, format!("file_{}_backup.txt", i)),
                _ => ("downloads", format!("file_{}_duplicate.txt", i)),
            };

            let dup_path = base_path.join(dup_dir).join(dup_name);
            fs::write(&dup_path, &content)
                .context(format!("Failed to write {}", dup_path.display()))?;
            total_files += 1;
            total_duplicates += 1;
        }
    }

    // Add some unique files (no duplicates) for variety
    let unique_files = vec![
        ("documents/readme.txt", "This is a unique readme file"),
        ("projects/project1/TODO.txt", "TODO: Implement features"),
        ("pictures/metadata.json", r#"{"camera": "Canon", "date": "2024-01-15"}"#),
    ];

    for (path, content) in unique_files {
        let file_path = base_path.join(path);
        fs::write(&file_path, content)?;
        total_files += 1;
    }

    // Print summary
    println!("\n✓ Demo data created successfully!");
    println!("\n📊 Summary:");
    println!("  Total files created: {}", total_files);
    println!("  Unique files: {}", num_files + unique_files.len());
    println!("  Duplicate files: {}", total_duplicates);
    println!("  Expected duplicate groups: {}", num_files);
    println!("\n📁 Directory structure:");
    println!("  {}/", base_path.display());
    println!("    ├── documents/       (original files)");
    println!("    ├── pictures/        (original files)");
    println!("    ├── downloads/       (duplicates - high deletion priority)");
    println!("    ├── temp/            (duplicates - highest deletion priority)");
    println!("    ├── backup/          (duplicates with 'copy' in name)");
    println!("    │   └── old/         (deeper duplicates)");
    println!("    └── projects/        (original and backup files)");
    println!("\n🎯 Test suggestions:");
    println!("  - Files in temp/ and downloads/ should score highest for deletion");
    println!("  - Files with 'copy', 'backup', 'duplicate' in name should be suggested");
    println!("  - Deeper nested files (backup/old/) should be suggested");
    println!("  - Original files in documents/ and projects/ should be kept");
    println!("\n▶  Run scan with:");
    println!("     dupscanner scan {}", base_path.display());

    Ok(())
}

fn generate_content(template: &str, target_size: usize, seed: usize) -> String {
    let mut content = String::new();
    content.push_str(&format!("=== {} ===\n\n", template));
    content.push_str(&format!("File ID: {}\n", seed));
    content.push_str(&format!("Generated for dupscanner demo\n\n"));

    // Pad to target size with repeating content
    let padding = "Lorem ipsum dolor sit amet, consectetur adipiscing elit. ";
    while content.len() < target_size {
        content.push_str(padding);
        content.push_str(&format!("Line {}\n", content.len() / 60));
    }

    content.truncate(target_size);
    content
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn test_generate_demo_data() {
        let temp_dir = TempDir::new().unwrap();
        let result = generate_demo_data(temp_dir.path(), 3, 2);
        assert!(result.is_ok());

        // Check that directories were created
        assert!(temp_dir.path().join("documents").exists());
        assert!(temp_dir.path().join("downloads").exists());
        assert!(temp_dir.path().join("temp").exists());
    }

    #[test]
    fn test_generate_content() {
        let content = generate_content("test", 500, 42);
        assert!(content.len() <= 500);
        assert!(content.contains("test"));
        assert!(content.contains("File ID: 42"));
    }
}
