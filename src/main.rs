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
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Version")]
    version: String,
    #[serde(rename = "Description")]
    description: Option<String>,
    #[serde(rename = "URL")]
    url: Option<String>,
    #[serde(rename = "NumVotes")]
    num_votes: Option<u32>,
}

/// Searches the AUR and return the response as a struct.
async fn search_aur(query: &str) -> Result<AurResponse, Box<dyn Error>> {
    // Create the AUR RPC search url.
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
fn is_package_installed(package: &str, debug: bool) -> bool {
    let result = Command::new("pacman")
        .args(["-Q", package])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map_or(false, |status| status.success());

    if debug {
        println!("Debug: Checking {} - installed: {}", package, result);
    }
    result
}

/// Downloads the .SRCINFO file for the provided package from AUR and returns the content.
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

/// Parses the .SRCINFO content to extract a list of dependency names and search for line with "depends ="
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

/// Clones the AUR repository for a package. (Will be cloned to `/home/$USER/.cache/aurorus/<package name>`).
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
/// Do the same thing with search first, but then prompt the user to select a package to install.
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
    combined_packages.sort_by_key(|pkg| pkg.num_votes.unwrap_or(0));

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
        println!("{}. {} (version: {}, Votes: {})", current_index, pkg.name, pkg.version, pkg.num_votes.unwrap_or(0));
        if let Some(desc) = &pkg.description {
            println!("   description: {}", desc);
        }
        if let Some(url) = &pkg.url {
            println!("   url: {}", url);
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
                    println!("{}. {} (version: {}, Repository: {})", current_index, name, version, repo);
                    if let Some((_, desc_line)) = lines.next() {
                        println!("   description: {}", desc_line.trim());
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
        &combined_packages[reversed_selection - 1].name
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

/// Updates packages
async fn update_packages() -> Result<(), Box<dyn Error>> {
    use futures::stream::{self, StreamExt};

    // Get installed AUR packages
    let output = Command::new("pacman")
        .args(["-Qm"])
        .output()?;

    let installed = String::from_utf8_lossy(&output.stdout);
        let packages: Vec<(String, String)> = installed
            .lines()
            .filter_map(|line| {
                let parts: Vec<&str> = line.split_whitespace().collect();
                if parts.len() >= 2 {
                    Some((parts[0].to_string(), parts[1].to_string()))
                } else {
                    None
                }
            })
            .collect();

    if packages.is_empty() {
        println!("No AUR packages installed.");
        return Ok(());
    }

    println!("Checking {} AUR package(s)...", packages.len());

    // Create chunks of package names for bulk RPC requests
    let chunk_size = 50; // AUR allows up to 50 packages per request
    let packages_chunks: Vec<Vec<String>> = packages
        .chunks(chunk_size)
        .map(|chunk| chunk.iter().map(|(name, _)| name.clone()).collect())
        .collect();

    // Process chunks in parallel
    let mut updates_available = Vec::new();
    let client = reqwest::Client::new();

    let results = stream::iter(packages_chunks)
        .map(|chunk| {
            let client = &client;
            async move {
                let names = chunk.join("&arg[]=");
                let url = format!("https://aur.archlinux.org/rpc/?v=5&type=info&arg[]={}", names);
                client.get(&url).send().await?.json::<AurResponse>().await
            }
        })
        .buffer_unordered(4) // Process 4 chunks concurrently
        .collect::<Vec<_>>()
        .await;

    use version_compare::Version;

    for result in results {
        if let Ok(response) = result {
            if let Some(aur_packages) = response.results {
                for aur_pkg in aur_packages {
                    if let Some((_, local_ver)) = packages.iter().find(|(name, _)| name == &aur_pkg.name) {
                        let ver_local = Version::from(local_ver);
                        let ver_aur = Version::from(&aur_pkg.version);
                        if let Some(ver_local) = ver_local {
                            if let Some(ver_aur) = ver_aur {
                                if ver_local < ver_aur {
                                    updates_available.push((aur_pkg.name, local_ver.clone(), aur_pkg.version));
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    if updates_available.is_empty() {
        println!("All packages are up to date!");
        return Ok(());
    }

    println!("\nUpdates available for {} package(s):", updates_available.len());
    for (i, (pkg, current, new)) in updates_available.iter().enumerate() {
        println!("{}. {} ({} -> {})", i + 1, pkg, current, new);
    }

    println!("\nEnter package numbers to update (e.g., '1 2 3'), or 'all' for all packages:");
    let mut input = String::new();
    io::stdin().read_line(&mut input)?;

    let to_update: Vec<(String, String, String)> = if input.trim().to_lowercase() == "all" {
        updates_available
    } else {
        let selected: Vec<usize> = input
            .split_whitespace()
            .filter_map(|s| s.parse::<usize>().ok())
            .filter(|&n| n > 0 && n <= updates_available.len())
            .collect();

        selected.iter()
            .filter_map(|&i| updates_available.get(i - 1).cloned())
            .collect()
    };

    for (package, _, _) in to_update {
        println!("\nUpdating {}...", package);
        let cache_dir = format!("/home/{}/.cache/aurorus", env::var("USER").unwrap_or_else(|_| "user".to_string()));
        let pkg_path = format!("{}/{}", cache_dir, package);

        clone_package_repo(&package).await?;

        let install_status = TokioCommand::new("makepkg")
            .args(["-si", "--noconfirm"])
            .current_dir(&pkg_path)
            .status()
            .await?;

        if install_status.success() {
            println!("{} updated successfully", package);
        } else {
            eprintln!("Failed to update {}", package);
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
    println!("  update, up             Update installed AUR packages.");
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
                    println!("Usage: search <package> or s <package> [-d]");
                } else {
                    let debug_mode = args.contains(&"-d");
                    let query = args.iter()
                            .filter(|arg| **arg != "-d")
                            .cloned()
                            .collect::<Vec<_>>()
                            .join(" ");

                    match search_aur(&query).await {
                        Ok(aur_response) => {
                            let mut combined_packages = if let Some(aur_packages) = aur_response.results {
                                aur_packages
                            } else {
                                vec![]
                            };

                            // Sort AUR packages by votes
                            combined_packages.sort_by(|a, b| a.num_votes.cmp(&b.num_votes));

                            // get the total count of packages to calculate reverse numbering
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

                            // Display AUR packages, sorted by votes, high to low
                            // Display AUR packages, sorted by votes, high to low
                            for pkg in combined_packages.iter() {
                                let installed = if is_package_installed(&pkg.name, debug_mode) {
                                    " (Installed)"
                                } else {
                                    ""
                                };
                                println!("{}. {} (v{}){}", current_index, pkg.name, pkg.version, installed);
                                if let Some(desc) = &pkg.description {
                                    println!("   description: {}", desc);
                                }
                                println!("   Votes: {}", pkg.num_votes.unwrap_or(0));
                                println!("-------------------------");
                                current_index -= 1;
                            }

                            // Show pkgs from official repos
                            let mut lines = official_packages.iter().enumerate();
                            while let Some((_, line)) = lines.next() {
                                if !line.starts_with(char::is_whitespace) {
                                    if let Some(repo_start) = line.find('[') {
                                        let package_info = &line[..repo_start].trim();
                                        let parts: Vec<&str> = package_info.split_whitespace().collect();
                                        if !parts.is_empty() {
                                            let name = parts[0];
                                            let version = parts.get(1).unwrap_or(&"");
                                            // Extract just the package name without repo prefix
                                            let pkg_name = name.split('/').last().unwrap_or(name);
                                            let installed = if is_package_installed(pkg_name, debug_mode) {
                                                " (Installed)"
                                            } else {
                                                ""
                                            };
                                            println!("{}. {} (v{}){}",
                                                current_index,
                                                name,
                                                version,
                                                installed
                                            );
                                            if let Some((_, desc_line)) = lines.next() {
                                                println!("   Description: {}", desc_line.trim());
                                            }
                                            println!("-------------------------");
                                            current_index -= 1;
                                        }
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

                    // Add debug pkgs to remove list, if installed along with main pkgs
                    if is_package_installed(&package, false) {
                        packages.push(&package);
                    }
                    if is_package_installed(&debug_package, false) {
                        packages.push(&debug_package);
                    }

                    if packages.is_empty() {
                        println!("No packages found to uninstall.");
                        continue;
                    }

                    // Remove packages with dependency cleanup
                    let status = Command::new("sudo")
                        .arg("pacman")
                        .arg("-Rns")  // -Rns removes package, dependencies, and config files
                        .args(&packages)
                        .status();

                    match status {
                        Ok(exit_status) if exit_status.success() => {
                            println!("Packages and dependencies removed successfully.");
                        }
                        Ok(_) => {
                            eprintln!("Failed to remove packages.");
                        }
                        Err(err) => {
                            eprintln!("Error removing packages: {}", err);
                        }
                    }
                }
            }
            "update" | "up" => {
                if let Err(err) = update_packages().await {
                    eprintln!("Error updating packages: {}", err);
                }
            }
            _ => {
                println!("Unknown command: {}. Type 'help' to see available commands.", command);
            }
        }
    }

    Ok(())
}
