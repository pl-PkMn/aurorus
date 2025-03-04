use futures::stream::{self, StreamExt};
use reqwest::Client;
use serde::Deserialize;
use std::env;
use std::error::Error as StdError;
use std::fmt;
use std::io::{self, Write};
use std::path::Path;
use std::process::Command;
use tokio::fs;
use tokio::process::Command as TokioCommand;
use version_compare::Version;

mod types {
    use serde::Deserialize;

    #[derive(Debug, Deserialize)]
    pub struct AurResponse {
        pub version: u8,
        #[serde(rename = "type")]
        pub resp_type: String,
        pub resultcount: u32,
        pub results: Option<Vec<AurPackage>>,
    }

    #[derive(Debug, Deserialize, Clone)]
    pub struct AurPackage {
        #[serde(rename = "Name")]
        pub name: String,
        #[serde(rename = "Version")]
        pub version: String,
        #[serde(rename = "Description")]
        pub description: Option<String>,
        #[serde(rename = "URL")]
        pub url: Option<String>,
        #[serde(rename = "NumVotes")]
        pub num_votes: Option<u32>,
    }
}

use types::*;

#[derive(Debug)]
enum AurorusError {
    Network(reqwest::Error),
    Io(io::Error),
    HttpStatus(String),
    OtherError(String),
}

impl fmt::Display for AurorusError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Network(e) => write!(f, "Network error: {}", e),
            Self::Io(e) => write!(f, "I/O error: {}", e),
            Self::HttpStatus(s) => write!(f, "HTTP error: {}", s),
            Self::OtherError(s) => write!(f, "{}", s),
        }
    }
}

impl StdError for AurorusError {}

impl From<reqwest::Error> for AurorusError {
    fn from(error: reqwest::Error) -> Self {
        AurorusError::Network(error)
    }
}

impl From<io::Error> for AurorusError {
    fn from(error: io::Error) -> Self {
        AurorusError::Io(error)
    }
}

impl From<String> for AurorusError {
    fn from(error: String) -> Self {
        AurorusError::OtherError(error)
    }
}

impl From<&str> for AurorusError {
    fn from(error: &str) -> Self {
        AurorusError::OtherError(error.to_string())
    }
}

type Result<T> = std::result::Result<T, AurorusError>;

mod aur {
    use super::*;

    pub async fn search(client: &Client, query: &str) -> Result<AurResponse> {
        let url = format!(
            "https://aur.archlinux.org/rpc/?v=5&type=search&arg={}",
            query
        );
        let resp = client.get(&url).send().await?;

        if !resp.status().is_success() {
            return Err(AurorusError::HttpStatus(resp.status().to_string()));
        }

        Ok(resp.json().await?)
    }

    pub async fn fetch_srcinfo(client: &Client, package: &str) -> Result<String> {
        let url = format!(
            "https://aur.archlinux.org/cgit/aur.git/plain/.SRCINFO?h={}",
            package
        );
        let resp = client.get(&url).send().await?;

        if !resp.status().is_success() {
            return Err(AurorusError::HttpStatus(format!(
                "Failed to fetch .SRCINFO for package {}: HTTP {}",
                package,
                resp.status()
            )));
        }

        Ok(resp.text().await?)
    }

    pub fn parse_dependencies(srcinfo: &str) -> Vec<String> {
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

    pub async fn clone_package_repo(package: &str) -> Result<String> {
        let repo_url = format!("https://aur.archlinux.org/{}.git", package);
        let cache_dir = format!(
            "/home/{}/.cache/aurorus",
            env::var("USER").unwrap_or_else(|_| "user".to_string())
        );
        let dest = format!("{}/{}", cache_dir, package);

        if !Path::new(&cache_dir).exists() {
            fs::create_dir_all(&cache_dir).await?;
        }

        if Path::new(&dest).exists() {
            println!("Directory {} already exists. Removing...", dest);
            fs::remove_dir_all(&dest).await?;
        }

        println!("Cloning {} into {} ...", repo_url, dest);
        let status = Command::new("git")
            .args(&["clone", &repo_url, &dest])
            .status()?;

        if !status.success() {
            return Err(format!("Failed to clone repository for {}.", package).into());
        }

        Ok(dest)
    }
}

mod pacman {
    use super::*;

    pub fn search(query: &str) -> Vec<String> {
        let output = Command::new("pacman")
            .arg("-Ss")
            .arg(query)
            .output()
            .expect("Failed to execute pacman search");

        let stdout = String::from_utf8_lossy(&output.stdout);
        stdout.lines().map(|line| line.to_string()).collect()
    }

    pub fn is_installed(package: &str, debug: bool) -> bool {
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

    pub fn get_installed_aur_packages() -> Result<Vec<(String, String)>> {
        let output = Command::new("pacman").args(["-Qm"]).output()?;

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

        Ok(packages)
    }
}

mod display {
    use super::*;

    pub fn print_package(index: usize, pkg: &AurPackage) {
        let installed = if pacman::is_installed(&pkg.name, false) {
            " (Installed)"
        } else {
            ""
        };
        println!("{}. {} ({}){}", index, pkg.name, pkg.version, installed);
        if let Some(desc) = &pkg.description {
            println!("   description: {}", desc);
        }
        println!("   Votes: {}", pkg.num_votes.unwrap_or(0));
        println!("-------------------------");
    }

    pub fn print_official_pkg(index: usize, line: &str, description: Option<&str>) {
        if let Some(repo_start) = line.find('[') {
            let parts: Vec<&str> = line[..repo_start].trim().split_whitespace().collect();
            if !parts.is_empty() {
                let name = parts[0];
                let version = parts.get(1).unwrap_or(&"");
                let pkg_name = name.split('/').last().unwrap_or(name);
                let installed = if pacman::is_installed(pkg_name, false) {
                    " (Installed)"
                } else {
                    ""
                };
                println!("{}. {} ({}){}", index, name, version, installed);
                if let Some(desc) = description {
                    println!("   description: {}", desc);
                }
            }
        }
        println!("-------------------------");
    }

    pub fn print_help() {
        println!("Available commands:");
        println!(
            "  search, s <package>     Search for a package in the AUR and official repositories (sorted by votes)."
        );
        println!(
            "  install, i <package>    Install a package from the AUR or official repositories (checks dependencies, clones, builds)."
        );
        println!("  uninstall, ui <package> Uninstall a package.");
        println!("  update, up              Update installed AUR packages and official packages.");
        println!("  help                    Show this help message.");
        println!("  exit                    Exit the application.");
    }
}

mod actions {
    use super::*;

    pub async fn search_packages(client: &Client, query: &str, debug_mode: bool) -> Result<()> {
        let aur_response = aur::search(client, query).await?;
        let mut combined_packages = aur_response.results.unwrap_or_default();

        // Sort AUR packages by votes
        combined_packages.sort_by(|a, b| a.num_votes.cmp(&b.num_votes));

        // Get official packages
        let official_packages = pacman::search(query);
        let mut total_count = 0;

        // Count official packages
        for line in official_packages.iter() {
            if !line.starts_with(char::is_whitespace) && line.find('[').is_some() {
                total_count += 1;
            }
        }
        total_count += combined_packages.len();

        let mut current_index = total_count;

        // Display AUR packages
        for pkg in combined_packages.iter() {
            display::print_package(current_index, pkg);
            current_index -= 1;
        }

        // Display official packages
        let mut lines = official_packages.iter().peekable();
        while let Some(line) = lines.next() {
            if !line.starts_with(char::is_whitespace) {
                let description = lines
                    .next()
                    .filter(|desc_line| desc_line.starts_with(char::is_whitespace))
                    .map(|desc_line| desc_line.trim());

                display::print_official_pkg(current_index, line, description);
                current_index -= 1;
            }
        }

        Ok(())
    }

    pub async fn get_missing_dependencies(client: &Client, package: &str) -> Result<Vec<String>> {
        println!("Fetching .SRCINFO for {}...", package);
        let srcinfo = aur::fetch_srcinfo(client, package).await?;
        let deps = aur::parse_dependencies(&srcinfo);

        if deps.is_empty() {
            println!("No dependencies found for {}.", package);
            return Ok(vec![]);
        }

        println!("Found dependencies:");
        for dep in &deps {
            println!("  {}", dep);
        }

        println!("\nChecking for missing dependencies:");
        let mut missing = Vec::new();
        for dep in deps {
            if !pacman::is_installed(&dep, false) {
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
        } else {
            println!("All dependencies for {} are satisfied.", package);
        }

        Ok(missing)
    }

    pub async fn install_aur_dependency(client: &Client, dep: &str) -> Result<()> {
        println!("Installing dependency {}...", dep);
        let package_dir = aur::clone_package_repo(dep).await?;
        let mut child = TokioCommand::new("makepkg")
            .args(&["-si", "--noconfirm"])
            .current_dir(&package_dir)
            .spawn()?;

        let status = child.wait().await?;
        if status.success() {
            println!("Dependency {} installed successfully.", dep);
        } else {
            eprintln!("Installation of dependency {} failed.", dep);
        }

        Ok(())
    }

    pub async fn install_package(client: &Client, query: &str) -> Result<()> {
        // Step 1: Search for packages
        let aur_response = aur::search(client, query).await?;
        let official_packages = pacman::search(query);

        // Step 2: Combine and process packages
        let mut combined_packages = aur_response.results.unwrap_or_default();
        combined_packages.sort_by_key(|pkg| pkg.num_votes.unwrap_or(0));

        // Calculate total packages
        let mut total_official_count = 0;
        for line in official_packages.iter() {
            if !line.starts_with(char::is_whitespace) && line.find('[').is_some() {
                total_official_count += 1;
            }
        }

        let total_count = total_official_count + combined_packages.len();
        let mut current_index = total_count;

        // Display AUR packages
        println!("Found {} package(s) in AUR:", combined_packages.len());
        for pkg in combined_packages.iter() {
            display::print_package(current_index, pkg);
            current_index -= 1;
        }

        // Display official packages
        // Display official packages
        let mut lines = official_packages.iter().peekable();
        while let Some(line) = lines.next() {
            if !line.starts_with(char::is_whitespace) {
                let description = lines
                    .next()
                    .filter(|desc_line| desc_line.starts_with(char::is_whitespace))
                    .map(|desc_line| desc_line.trim());

                display::print_official_pkg(current_index, line, description);
                current_index -= 1;
            }
        }

        // Step 3: Prompt user to select a package
        println!("\nEnter the number of the package to install (or type 'back' to cancel):");
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if input.eq_ignore_ascii_case("back") || input.eq_ignore_ascii_case("quit") {
            println!("Exiting installation process.");
            return Ok(());
        }

        let selection: usize = match input.parse() {
            Ok(num) if (1..=total_count).contains(&num) => num,
            _ => {
                return Err(format!(
                    "Invalid selection. Please enter a number between 1 and {}.",
                    total_count
                )
                .into());
            }
        };

        // Adjust selection to match the reversed numbering
        let reversed_selection = total_count - selection + 1;
        let selected_package = if reversed_selection <= combined_packages.len() {
            &combined_packages[reversed_selection - 1].name
        } else {
            // Extract the package name from the official repository
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

        // Check dependencies and get missing ones
        let missing_deps = get_missing_dependencies(client, selected_package).await?;
        if !missing_deps.is_empty() {
            println!("\nDo you want to install the missing dependencies? (y/N):");
            let mut dep_input = String::new();
            io::stdin().read_line(&mut dep_input)?;

            if dep_input.trim().eq_ignore_ascii_case("y") {
                for dep in missing_deps {
                    install_aur_dependency(client, &dep).await?;
                }
            } else {
                println!("Proceeding without installing missing dependencies.");
            }
        }

        // Install the selected package
        if selection <= combined_packages.len() {
            let package_dir = aur::clone_package_repo(selected_package).await?;
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

    pub async fn update_packages(client: &Client) -> Result<()> {
        // Get installed AUR packages
        let packages = pacman::get_installed_aur_packages()?;

        if packages.is_empty() {
            println!("No AUR packages installed.");
        } else {
            println!("Checking {} AUR package(s)...", packages.len());

            // Create chunks for bulk RPC requests
            let chunk_size = 50; // AUR allows up to 50 packages per request
            let packages_chunks: Vec<Vec<String>> = packages
                .chunks(chunk_size)
                .map(|chunk| chunk.iter().map(|(name, _)| name.clone()).collect())
                .collect();

            // Process chunks in parallel
            let mut updates_available = Vec::new();

            let results = stream::iter(packages_chunks)
                .map(|chunk| {
                    let client = client.clone();
                    async move {
                        let names = chunk.join("&arg[]=");
                        let url = format!(
                            "https://aur.archlinux.org/rpc/?v=5&type=info&arg[]={}",
                            names
                        );
                        client.get(&url).send().await?.json::<AurResponse>().await
                    }
                })
                .buffer_unordered(4)
                .collect::<Vec<_>>()
                .await;

            for result in results {
                if let Ok(response) = result {
                    if let Some(aur_packages) = response.results {
                        for aur_pkg in aur_packages {
                            if let Some((_, local_ver)) =
                                packages.iter().find(|(name, _)| name == &aur_pkg.name)
                            {
                                let ver_local = Version::from(local_ver);
                                let ver_aur = Version::from(&aur_pkg.version);
                                if let (Some(ver_local), Some(ver_aur)) = (ver_local, ver_aur) {
                                    if ver_local < ver_aur {
                                        updates_available.push((
                                            aur_pkg.name,
                                            local_ver.clone(),
                                            aur_pkg.version,
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if updates_available.is_empty() {
                println!("No available update for AUR packages.");
            } else {
                println!(
                    "\nUpdates available for {} package(s):",
                    updates_available.len()
                );
                for (i, (pkg, current, new)) in updates_available.iter().enumerate() {
                    println!("{}. {} ({} -> {})", i + 1, pkg, current, new);
                }

                println!(
                    "\nEnter package numbers to update (e.g., '1 2 3'), press Enter to update all packages, or type 'back' to cancel:"
                );
                let mut input = String::new();
                io::stdin().read_line(&mut input)?;
                let input = input.trim();

                if input.eq_ignore_ascii_case("back") || input.eq_ignore_ascii_case("quit") {
                    println!("Exiting update process.");
                    return Ok(());
                }

                let to_update: Vec<(String, String, String)> =
                    if input.is_empty() || input.to_lowercase() == "all" {
                        updates_available
                    } else {
                        let selected: Vec<usize> = input
                            .split_whitespace()
                            .filter_map(|s| s.parse::<usize>().ok())
                            .filter(|&n| n > 0 && n <= updates_available.len())
                            .collect();

                        selected
                            .iter()
                            .filter_map(|&i| updates_available.get(i - 1).cloned())
                            .collect()
                    };

                for (package, _, _) in to_update {
                    println!("\nUpdating {}...", package);
                    let pkg_path = aur::clone_package_repo(&package).await?;

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
            }
        }

        // Update official packages
        println!("\nUpdating official packages via pacman...");
        let official_status = Command::new("sudo").arg("pacman").arg("-Syu").status()?;

        if official_status.success() {
            println!("Official packages updated successfully.");
        } else {
            eprintln!("Failed to update official packages.");
        }

        Ok(())
    }

    pub fn uninstall_package(packages: Vec<&str>) -> Result<()> {
        if packages.is_empty() {
            println!("No packages found to uninstall.");
            return Ok(());
        }

        let status = Command::new("sudo")
            .arg("pacman")
            .arg("-Rns")
            .args(&packages)
            .status()?;

        if status.success() {
            println!("Packages and dependencies removed successfully.");
            Ok(())
        } else {
            Err("Failed to remove packages".into())
        }
    }
}

fn read_user_input() -> io::Result<String> {
    let mut input = String::new();
    print!("aurorus> ");
    io::stdout().flush()?;
    io::stdin().read_line(&mut input)?;
    Ok(input.trim().to_string())
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn StdError>> {
    println!("Welcome to aurorus!");
    println!("Type 'help' for a list of commands.\n");

    // Create a reusable HTTP client
    let client = Client::new();

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
                display::print_help();
            }
            "search" | "s" => {
                if args.is_empty() {
                    println!("Usage: search <package> or s <package> [-d]");
                } else {
                    let debug_mode = args.contains(&"-d");
                    let query = args
                        .iter()
                        .filter(|arg| **arg != "-d")
                        .cloned()
                        .collect::<Vec<_>>()
                        .join(" ");

                    if let Err(e) = actions::search_packages(&client, &query, debug_mode).await {
                        eprintln!("Error searching for package: {}", e);
                    }
                }
            }
            "install" | "i" => {
                if args.is_empty() {
                    println!("Usage: install <package> or i <package>");
                } else {
                    let query = args.join(" ");
                    if let Err(e) = actions::install_package(&client, &query).await {
                        eprintln!("Error installing package: {}", e);
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

                    // Add debug pkgs to remove list if installed
                    if pacman::is_installed(&package, false) {
                        packages.push(package.as_str());
                    }
                    if pacman::is_installed(&debug_package, false) {
                        packages.push(debug_package.as_str());
                    }

                    if let Err(e) = actions::uninstall_package(packages) {
                        eprintln!("Error uninstalling packages: {}", e);
                    }
                }
            }
            "update" | "up" => {
                if let Err(e) = actions::update_packages(&client).await {
                    eprintln!("Error updating packages: {}", e);
                }
            }
            _ => {
                println!(
                    "Unknown command: {}. Type 'help' to see available commands.",
                    command
                );
            }
        }
    }

    Ok(())
}
