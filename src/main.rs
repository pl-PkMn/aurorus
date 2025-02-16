use std::env;
use reqwest;
use serde::Deserialize;
use std::error::Error;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;
use tokio::fs;
use tokio::process::Command as TokioCommand;

#[derive(Debug, Deserialize)]
struct AurResponse {
    version: u8,
    #[serde(rename = "type")]
    resp_type: String,
    resultcount: u32,
    results: Option<Vec<AurPackage>>,
}

#[derive(Debug, Deserialize)]
struct AurPackage {
    Name: String,
    Version: String,
    Description: Option<String>,
    URL: Option<String>,
    #[serde(rename = "NumVotes")]
    NumVotes: Option<u32>,
    // Add additional fields as needed.
}

/// Searches the AUR for a package using the provided query string.
/// Returns the list of packages found.
async fn search_aur(query: &str) -> Result<AurResponse, Box<dyn Error>> {
    // Create the AUR RPC search URL.
    let url = format!("https://aur.archlinux.org/rpc/?v=5&type=search&arg={}", query);
    let resp = reqwest::get(&url).await?;

    if !resp.status().is_success() {
        eprintln!("HTTP request failed: {}", resp.status());
        return Err("HTTP request failed".into());
    }

    let aur_response: AurResponse = resp.json().await?;
    Ok(aur_response)
}

/// Searches the official repositories for a package using pacman.
fn search_official_repos(query: &str) -> Vec<String> {
    let output = Command::new("pacman")
        .arg("-Ss")
        .arg(query)
        .output()
        .expect("Failed to execute pacman search");

    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout.lines().map(|line| line.to_string()).collect()
}

/// Downloads the .SRCINFO file for the provided package from AUR.
/// Returns the content as a String.
async fn fetch_srcinfo(package: &str) -> Result<String, Box<dyn Error>> {
    let url = format!("https://aur.archlinux.org/cgit/aur.git/plain/.SRCINFO?h={}", package);
    let resp = reqwest::get(&url).await?;

    if !resp.status().is_success() {
        Err(format!(
            "Failed to fetch .SRCINFO for package {}: HTTP {}",
            package,
            resp.status()
        ))?
    } else {
        let content = resp.text().await?;
        Ok(content)
    }
}

/// Parses the .SRCINFO content to extract a list of dependency names.
/// It searches for lines starting with "depends =".
fn parse_dependencies(srcinfo: &str) -> Vec<String> {
    let mut deps = Vec::new();
    for line in srcinfo.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("depends =") {
            if let Some(dep) = trimmed.split('=').nth(1) {
                deps.push(dep.trim().to_string());
            }
        }
    }
    deps
}

/// Checks whether a dependency is installed by calling "pacman -Q <dep>".
/// Returns true if installed, false otherwise.
fn check_dependency(dep: &str) -> bool {
    let status = Command::new("pacman").arg("-Q").arg(dep).status();
    match status {
        Ok(exit_status) => exit_status.success(),
        Err(_) => false,
    }
}

/// Checks dependencies for the provided package:
/// 1. Fetches the .SRCINFO file.
/// 2. Parses the dependencies.
/// 3. Checks and prints which dependencies are missing.
async fn check_dependencies(package: &str) -> Result<(), Box<dyn Error>> {
    println!("Fetching .SRCINFO for {}...", package);
    let srcinfo = fetch_srcinfo(package).await?;
    let deps = parse_dependencies(&srcinfo);

    if deps.is_empty() {
        println!("No dependencies found for {}.", package);
        return Ok(());
    }

    println!("Found dependencies:");
    for dep in &deps {
        println!("  {}", dep);
    }
    println!("\nChecking for missing dependencies:");

    let mut missing = Vec::new();
    for dep in deps {
        if !check_dependency(&dep) {
            println!("  {} is missing.", dep);
            missing.push(dep);
        } else {
            println!("  {} is installed.", dep);
        }
    }

    if !missing.is_empty() {
        println!("\nMissing dependencies:");
        for dep in &missing {
            println!("  {}", dep);
        }
        println!("Please install the missing dependencies before proceeding.");
    } else {
        println!("All dependencies for {} are satisfied.", package);
    }

    Ok(())
}

/// Clones the AUR repository for a package.
/// The repository URL is typically https://aur.archlinux.org/<package>.git
/// The package will be cloned into `/home/$USER/.cache/aurorus/<package name>`.
async fn clone_package_repo(package: &str) -> Result<String, Box<dyn Error>> {
    let repo_url = format!("https://aur.archlinux.org/{}.git", package);
    let cache_dir = format!("/home/{}/.cache/aurorus", env::var("USER").unwrap_or_else(|_| "user".to_string()));
    let dest = format!("{}/{}", cache_dir, package);

    // Ensure the cache directory exists.
    if !Path::new(&cache_dir).exists() {
        fs::create_dir_all(&cache_dir).await?;
    }

    // Remove the destination directory if it already exists.
    if Path::new(&dest).exists() {
        println!("Directory {} already exists. Removing...", dest);
        fs::remove_dir_all(&dest).await?;
    }

    // Use git to clone the repository.
    println!("Cloning {} into {} ...", repo_url, dest);
    let status = Command::new("git")
        .args(&["clone", &repo_url, &dest])
        .status()?;

    if !status.success() {
        Err(format!("Failed to clone repository for {}.", package))?
    }

    Ok(dest)
}

/// Installs the package:
/// 1. Searches for packages matching the query in both AUR and official repositories.
/// 2. Displays search results with numbers, sorted by votes.
/// 3. Prompts user to select a package.
/// 4. Checks dependencies, clones the repo, and runs "makepkg -si".
async fn install_package(query: &str) -> Result<(), Box<dyn Error>> {
    // Step 1: Search for packages matching the query in AUR.
    let aur_response = search_aur(query).await?;

    // Step 1.1: Search for packages matching the query in official repositories.
    let official_packages = search_official_repos(query);

    // Step 2: Combine and sort packages by votes.
    let mut combined_packages = if let Some(aur_packages) = aur_response.results {
        aur_packages
    } else {
        vec![]
    };

    // Sort AUR packages by number of votes in ascending order.
    combined_packages.sort_by_key(|pkg| pkg.NumVotes.unwrap_or(0));

    // Display AUR packages.
    println!("Found {} package(s) in AUR:", combined_packages.len());
    for (index, pkg) in combined_packages.iter().enumerate() {
        println!("{}. {} (Version: {}, Votes: {})", index + 1, pkg.Name, pkg.Version, pkg.NumVotes.unwrap_or(0));
        if let Some(desc) = &pkg.Description {
            println!("   Description: {}", desc);
        }
        if let Some(url) = &pkg.URL {
            println!("   URL: {}", url);
        }
        println!("-------------------------");
    }

    // Display official repository packages with similar formatting.
    if !official_packages.is_empty() {
        println!("\nFound packages in official repositories:");
        for (index, line) in official_packages.iter().enumerate() {
            // Extract package name, version, and repository from the pacman output
            let parts: Vec<&str> = line.split_whitespace().collect();
            if parts.len() >= 3 {
                let name = parts[0];
                let version = parts[1];
                let repo = parts[2].trim_end_matches(']');
                println!("{}. {} (Version: {}, Repository: {})", index + 1 + combined_packages.len(), name, version, repo);
                println!("-------------------------");
            } else {
                println!("{}. {}", index + 1 + combined_packages.len(), line);
                println!("-------------------------");
            }
        }
    } else {
        println!("No matching packages found in official repositories.");
    }

    // Step 3: Prompt user to select a package.
    println!("\nEnter the number of the package to install:");
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let selection: usize = match input.trim().parse() {
        Ok(num) if (1..=combined_packages.len() + official_packages.len()).contains(&num) => num,
        _ => {
            eprintln!("Invalid selection. Please enter a number between 1 and {}.", combined_packages.len() + official_packages.len());
            return Ok(());
        }
    };

    // Step 4: Install the selected package.
    let selected_package = if selection <= combined_packages.len() {
        &combined_packages[selection - 1].Name
    } else {
        // Extract the package name from the official repository search result
        official_packages[selection - 1 - combined_packages.len()].split_whitespace().next().unwrap()
    };

    println!("Installing {}...", selected_package);

    // Check dependencies.
    check_dependencies(selected_package).await?;

    // Clone the AUR repository if the package is from AUR.
    if selection <= combined_packages.len() {
        let package_dir = clone_package_repo(selected_package).await?;

        // Build and install the package.
        let mut child = TokioCommand::new("makepkg")
            .arg("-si")
            .current_dir(&package_dir)
            .spawn()?;

        let status = child.wait().await?;
        if status.success() {
            println!("Package {} installed successfully.", selected_package);
        } else {
            eprintln!("Installation of {} failed.", selected_package);
        }
    } else {
        // Install the package from official repositories using pacman.
        let status = Command::new("sudo")
            .arg("pacman")
            .arg("-S")
            .arg(selected_package)
            .status()?;

        if status.success() {
            println!("Package {} installed successfully.", selected_package);
        } else {
            eprintln!("Installation of {} failed.", selected_package);
        }
    }

    Ok(())
}

/// Displays a simple help text.
fn print_help() {
    println!("Available commands:");
    println!("  search <package>   Search for a package in the AUR and official repositories (sorted by votes).");
    println!("  install <package>  Install a package from the AUR or official repositories (checks dependencies, clones, builds).");
    println!("  help               Show this help message.");
    println!("  exit               Exit the application.");
}

/// Reads a trimmed line of input from STDIN.
fn read_user_input() -> io::Result<String> {
    let mut input = String::new();
    print!("aurorus> ");
    io::stdout().flush()?;
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_string())
}

/// Entry point for the interactive application.
#[tokio::main]
async fn main() -> Result<(), Box<dyn Error>> {
    println!("Welcome to aurorus!");
    println!("Type 'help' for a list of commands.\n");

    loop {
        let input = match read_user_input() {
            Ok(line) => line,
            Err(e) => {
                eprintln!("Error reading input: {}", e);
                continue;
            }
        };

        if input.is_empty() {
            continue;
        }

        let mut parts = input.split_whitespace();
        let command = parts.next().unwrap().to_lowercase();
        let args: Vec<&str> = parts.collect();

        match command.as_str() {
            "exit" => {
                println!("Exiting aurorus. Goodbye!");
                break;
            }
            "help" => {
                print_help();
            }
            "search" => {
                if args.is_empty() {
                    println!("Usage: search <package>");
                } else {
                    let query = args.join(" ");
                    match search_aur(&query).await {
                        Ok(aur_response) => {
                            let mut combined_packages = if let Some(aur_packages) = aur_response.results {
                                aur_packages
                            } else {
                                vec![]
                            };

                            // Sort AUR packages by number of votes in ascending order
                            combined_packages.sort_by_key(|pkg| pkg.NumVotes.unwrap_or(0));

                            let official_packages = search_official_repos(&query);
                            let total_packages = combined_packages.len() + (official_packages.len() / 2);
                            let mut current_index = 1;

                            // Display AUR packages with lower votes first (at the top)
                            for pkg in &combined_packages {
                                print!("{}. {} (Version: {}, Votes: {})", current_index, pkg.Name, pkg.Version, pkg.NumVotes.unwrap_or(0));
                                if let Some(desc) = &pkg.Description {
                                    print!(" - {}", desc);
                                }
                                if let Some(url) = &pkg.URL {
                                    print!(" [{}]", url);
                                }
                                println!();
                                current_index += 1;
                            }

                            // Display official repository packages at the bottom
                            let mut lines = official_packages.iter().peekable();
                            while let Some(line) = lines.next() {
                                if line.starts_with(char::is_whitespace) {
                                    continue;
                                }

                                let parts: Vec<&str> = line.split_whitespace().collect();
                                if parts.len() >= 3 {
                                    let name = parts[0];
                                    let version = parts[1];
                                    let repo = parts[2].trim_end_matches(']');

                                    print!("{}. {} (Version: {}, Repository: {})", current_index, name, version, repo);

                                    if let Some(next_line) = lines.peek() {
                                        if next_line.starts_with(char::is_whitespace) {
                                            print!(" - {}", next_line.trim());
                                        }
                                    }
                                    println!();
                                    current_index += 1;
                                }
                            }

                            println!("\nFound {} package(s)", total_packages);
                        }
                        Err(err) => {
                            eprintln!("Error searching for package: {}", err);
                        }
                    }
                }
            }
            "install" => {
                if args.is_empty() {
                    println!("Usage: install <package>");
                } else {
                    let query = args.join(" ");
                    if let Err(err) = install_package(&query).await {
                        eprintln!("Error installing package: {}", err);
                    }
                }
            }
            _ => {
                println!("Unknown command: {}. Type 'help' to see available commands.", command);
            }
        }
    }

    Ok(())
}
