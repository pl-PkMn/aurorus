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

/// Checks if a package is installed using pacman -Q
/// Checks if a package is installed using pacman -Q
fn is_package_installed(package: &str) -> bool {
    Command::new("pacman")
        .args(["-Q", package])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())  // Add this line to suppress stderr
        .status()
        .map_or(false, |status| status.success())
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

    // Sort AUR packages by votes (lowest first)
    combined_packages.sort_by_key(|pkg| pkg.NumVotes.unwrap_or(0));

    // Calculate total packages for numbering
    let mut total_official_count = 0;
    for line in official_packages.iter() {
        if !line.starts_with(char::is_whitespace) && line.find('[').is_some() {
            total_official_count += 1;
        }
    }
    let total_count = total_official_count + combined_packages.len();

    let mut current_index = total_count;

    // Display AUR packages.
    println!("Found {} package(s) in AUR:", combined_packages.len());
    for pkg in combined_packages.iter() {
        println!("{}. {} (Version: {}, Votes: {})", current_index, pkg.Name, pkg.Version, pkg.NumVotes.unwrap_or(0));
        if let Some(desc) = &pkg.Description {
            println!("   Description: {}", desc);
        }
        if let Some(url) = &pkg.URL {
            println!("   URL: {}", url);
        }
        println!("-------------------------");
        current_index -= 1;
    }

    // Display official repository packages.
    let mut lines = official_packages.iter().enumerate();
    while let Some((_, line)) = lines.next() {
        if !line.starts_with(char::is_whitespace) {
            if let Some(repo_start) = line.find('[') {
                let parts: Vec<&str> = line[..repo_start].trim().split_whitespace().collect();
                if !parts.is_empty() {
                    let name = parts[0];
                    let version = parts.get(1).unwrap_or(&"");
                    let repo = line[repo_start..].trim_matches(|c| c == '[' || c == ']');
                    println!("{}. {} (Version: {}, Repository: {})", current_index, name, version, repo);
                    if let Some((_, desc_line)) = lines.next() {
                        println!("   Description: {}", desc_line.trim());
                    }
                    println!("-------------------------");
                    current_index -= 1;
                }
            }
        }
    }

    // Step 3: Prompt user to select a package.
    println!("\nEnter the number of the package to install:",);
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;
    let selection: usize = match input.trim().parse() {
        Ok(num) if (1..=total_count).contains(&num) => num,
        _ => {
            eprintln!("Invalid selection. Please enter a number between 1 and {}.", total_count);
            return Ok(());
        }
    };

    // Adjust selection to match the reversed numbering
    let reversed_selection = total_count - selection + 1;

    // Step 4: Install the selected package.
    let selected_package = if reversed_selection <= combined_packages.len() {
        &combined_packages[reversed_selection - 1].Name
    } else {
        // Extract the package name from the official repository search result
        let mut count = 0;
        let mut package_name = "";
        for line in official_packages.iter() {
            if !line.starts_with(char::is_whitespace) && line.find('[').is_some() {
                count += 1;
                if count == (reversed_selection - combined_packages.len()) {
                    package_name = line.split_whitespace().next().unwrap_or("");
                    break;
                }
            }
        }
        package_name
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
    println!("  search, s <package>     Search for a package in the AUR and official repositories (sorted by votes).");
    println!("  install, i <package>    Install a package from the AUR or official repositories (checks dependencies, clones, builds).");
    println!("  uninstall, ui <package> Uninstall a package.");
    println!("  help                    Show this help message.");
    println!("  exit                    Exit the application.");
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
            "search" | "s" => {
                if args.is_empty() {
                    println!("Usage: search <package> or s <package>");
                } else {
                    let query = args.join(" ");
                    match search_aur(&query).await {
                        Ok(aur_response) => {
                            let mut combined_packages = if let Some(aur_packages) = aur_response.results {
                                aur_packages
                            } else {
                                vec![]
                            };

                            // Sort AUR packages by votes (lowest first)
                            combined_packages.sort_by(|a, b| a.NumVotes.cmp(&b.NumVotes));

                            // First, get the total count of packages to calculate reverse numbering
                            let official_packages = search_official_repos(&query);
                            let mut total_count = 0;

                            // Count official packages
                            for line in official_packages.iter() {
                                if !line.starts_with(char::is_whitespace) && line.find('[').is_some() {
                                    total_count += 1;
                                }
                            }
                            total_count += combined_packages.len();

                            let mut current_index = total_count;

                            // Display AUR packages (already sorted by votes, lowest first)
                            for pkg in combined_packages.iter() {
                                let installed = if is_package_installed(&pkg.Name) {
                                    " (Installed)"
                                } else {
                                    ""
                                };
                                println!("{}. {} (v{}){}", current_index, pkg.Name, pkg.Version, installed);
                                if let Some(desc) = &pkg.Description {
                                    println!("   Description: {}", desc);
                                }
                                println!("   Votes: {}", pkg.NumVotes.unwrap_or(0));
                                println!("-------------------------");
                                current_index -= 1;
                            }

                            // Display official repository packages (they will be shown first with lowest numbers)
                            let mut lines = official_packages.iter().enumerate();

                            while let Some((_, line)) = lines.next() {
                                if !line.starts_with(char::is_whitespace) {
                                    if let Some(repo_start) = line.find('[') {
                                        let package_info = &line[..repo_start].trim();
                                        let repo = line[repo_start..].trim_matches(|c| c == '[' || c == ']');

                                        let pkg_name = package_info.split_whitespace().next().unwrap_or("");
                                        let installed = if is_package_installed(pkg_name) {
                                            " (Installed)"
                                        } else {
                                            ""
                                        };

                                        println!("{}. {}{}", current_index, package_info, installed);
                                        println!("   Repository: {}", repo);

                                        if let Some((_, desc_line)) = lines.next() {
                                            println!("   Description: {}", desc_line.trim());
                                        }
                                        println!("-------------------------");
                                        current_index -= 1;
                                    }
                                }
                            }
                        }
                        Err(err) => {
                            eprintln!("Error searching for package: {}", err);
                        }
                    }
                }
            }
            "install" | "i" => {
                if args.is_empty() {
                    println!("Usage: install <package> or i <package>");
                } else {
                    let query = args.join(" ");
                    if let Err(err) = install_package(&query).await {
                        eprintln!("Error installing package: {}", err);
                    }
                }
            }
            "uninstall" | "ui" => {
                if args.is_empty() {
                    println!("Usage: uninstall <package> or ui <package>");
                } else {
                    let package = args.join(" ");
                    let debug_package = format!("{}-debug", package);
                    let mut packages = Vec::new();

                    // Add packages to remove list if they are installed
                    if is_package_installed(&package) {
                        packages.push(&package);
                    }
                    if is_package_installed(&debug_package) {
                        packages.push(&debug_package);
                    }

                    if packages.is_empty() {
                        println!("No packages found to uninstall.");
                        continue;
                    }

                    // Remove all packages in one command
                    let status = Command::new("sudo")
                        .arg("pacman")
                        .arg("-R")
                        .args(&packages)
                        .status();

                    match status {
                        Ok(exit_status) if exit_status.success() => {
                            println!("Packages uninstalled successfully.");
                        }
                        Ok(_) => {
                            eprintln!("Failed to uninstall packages.");
                        }
                        Err(err) => {
                            eprintln!("Error uninstalling packages: {}", err);
                        }
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
