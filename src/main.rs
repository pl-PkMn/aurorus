use futures::stream::{self, StreamExt};
use reqwest::Client;
use serde::Deserialize;
use std::{
    env, fmt, io::{self, Write},
    path::Path, process::Command,
    error::Error as StdError
};
use tokio::{fs, process::Command as TokioCommand};
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

// Simplified error handling
#[derive(Debug)]
enum AurorusError {
    Network(reqwest::Error),
    Io(io::Error),
    Message(String),
}

impl fmt::Display for AurorusError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        match self {
            Self::Network(e) => write!(f, "Network error: {}", e),
            Self::Io(e) => write!(f, "I/O error: {}", e),
            Self::Message(s) => write!(f, "{}", s),
        }
    }
}

impl StdError for AurorusError {}

impl From<reqwest::Error> for AurorusError {
    fn from(error: reqwest::Error) -> Self { AurorusError::Network(error) }
}

impl From<io::Error> for AurorusError {
    fn from(error: io::Error) -> Self { AurorusError::Io(error) }
}

// Replace generic implementation with more specific ones
impl From<&str> for AurorusError {
    fn from(error: &str) -> Self { AurorusError::Message(error.to_string()) }
}

impl From<String> for AurorusError {
    fn from(error: String) -> Self { AurorusError::Message(error) }
}

type Result<T> = std::result::Result<T, AurorusError>;

mod aur {
    use super::*;

    pub async fn search(client: &Client, query: &str) -> Result<AurResponse> {
        let url = format!("https://aur.archlinux.org/rpc/?v=5&type=search&arg={}", query);
        let resp = client.get(&url).send().await?;

        if !resp.status().is_success() {
            return Err(format!("HTTP error: {}", resp.status()).into());
        }

        Ok(resp.json().await?)
    }

    pub async fn fetch_srcinfo(client: &Client, package: &str) -> Result<String> {
        let url = format!("https://aur.archlinux.org/cgit/aur.git/plain/.SRCINFO?h={}", package);
        let resp = client.get(&url).send().await?;

        if !resp.status().is_success() {
            return Err(format!("Failed to fetch .SRCINFO for {}: HTTP {}", package, resp.status()).into());
        }

        Ok(resp.text().await?)
    }

    pub fn parse_dependencies(srcinfo: &str) -> Vec<String> {
        srcinfo.lines()
            .filter_map(|line| {
                let trimmed = line.trim();
                if trimmed.starts_with("depends =") {
                    trimmed.split('=').nth(1).map(|dep| dep.trim().to_string())
                } else {
                    None
                }
            })
            .collect()
    }

    pub async fn clone_package_repo(package: &str) -> Result<String> {
        let repo_url = format!("https://aur.archlinux.org/{}.git", package);
        let cache_dir = format!("/home/{}/.cache/aurorus",
                              env::var("USER").unwrap_or_else(|_| "user".to_string()));
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
            .args(["clone", &repo_url, &dest])
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
        Command::new("pacman")
            .arg("-Ss")
            .arg(query)
            .output()
            .map(|output| {
                String::from_utf8_lossy(&output.stdout)
                    .lines()
                    .map(|line| line.to_string())
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn is_installed(package: &str) -> bool {
        Command::new("pacman")
            .args(["-Q", package])
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status()
            .map_or(false, |status| status.success())
    }

    pub fn get_installed_aur_packages() -> Result<Vec<(String, String)>> {
        let output = Command::new("pacman").args(["-Qm"]).output()?;
        let installed = String::from_utf8_lossy(&output.stdout);

        let packages = installed
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
        let installed = if pacman::is_installed(&pkg.name) { " (Installed)" } else { "" };
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
                let installed = if pacman::is_installed(pkg_name) { " (Installed)" } else { "" };
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
        println!("  search, s <package>     Search for a package in the AUR and official repositories.");
        println!("  install, i <package>    Install a package from the AUR or official repositories.");
        println!("  uninstall, ui <package> Uninstall a package.");
        println!("  update, up              Update installed AUR packages and official packages.");
        println!("  help                    Show this help message.");
        println!("  exit                    Exit the application.");
    }
}

mod actions {
    use super::*;

    pub async fn search_packages(client: &Client, query: &str) -> Result<()> {
        // Process AUR results
        let aur_response = aur::search(client, query).await?;
        let mut aur_packages = aur_response.results.unwrap_or_default();

        // Sort by votes - ascending order (least votes first)
        aur_packages.sort_by(|a, b| a.num_votes.cmp(&b.num_votes));

        // Get official packages
        let official_packages = pacman::search(query);

        // Count official packages to determine numbering
        let mut official_count = 0;
        let mut lines = official_packages.iter().peekable();
        while let Some(line) = lines.peek() {
            if !line.starts_with(char::is_whitespace) && line.find('[').is_some() {
                official_count += 1;
            }
            lines.next();
        }

        // Set starting index for AUR packages (total packages count)
        let total_packages = aur_packages.len() + official_count;
        let mut index = total_packages;

        // Display AUR packages with decreasing indices
        for pkg in &aur_packages {
            display::print_package(index, pkg);
            index -= 1;
        }

        // Reset lines iterator and display official packages with continuing indices
        index = official_count;
        let mut lines = official_packages.iter().peekable();
        while let Some(line) = lines.next() {
            if !line.starts_with(char::is_whitespace) && line.find('[').is_some() {
                let description = lines
                    .next()
                    .filter(|desc_line| desc_line.starts_with(char::is_whitespace))
                    .map(|desc_line| desc_line.trim());

                display::print_official_pkg(index, line, description);
                index -= 1;
            }
        }

        Ok(())
    }

    async fn handle_dependencies(client: &Client, package: &str) -> Result<()> {
        println!("Fetching .SRCINFO for {}...", package);
        let srcinfo = aur::fetch_srcinfo(client, package).await?;
        let deps = aur::parse_dependencies(&srcinfo);

        if deps.is_empty() {
            println!("No dependencies found for {}.", package);
            return Ok(());
        }

        println!("Found dependencies:");
        let mut missing = Vec::new();
        for dep in &deps {
            let is_installed = pacman::is_installed(dep);
            println!("  {} {}", dep, if is_installed { "(installed)" } else { "(missing)" });
            if !is_installed {
                missing.push(dep.clone());
            }
        }

        if missing.is_empty() {
            println!("All dependencies for {} are satisfied.", package);
            return Ok(());
        }

        println!("\nDo you want to install {} missing dependencies? (y/N):", missing.len());
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;

        if input.trim().eq_ignore_ascii_case("y") {
            for dep in missing {
                println!("Installing dependency {}...", dep);
                let package_dir = aur::clone_package_repo(&dep).await?;

                let status = TokioCommand::new("makepkg")
                    .args(["-si", "--noconfirm"])
                    .current_dir(&package_dir)
                    .status()
                    .await?;

                if status.success() {
                    println!("Dependency {} installed successfully.", dep);
                } else {
                    eprintln!("Installation of dependency {} failed.", dep);
                }
            }
        } else {
            println!("Proceeding without installing missing dependencies.");
        }

        Ok(())
    }

    pub async fn install_package(client: &Client, query: &str) -> Result<()> {
        // Search for packages
        let aur_response = aur::search(client, query).await?;
        let mut aur_packages = aur_response.results.unwrap_or_default();
        let official_packages = pacman::search(query);

        // Sort AUR packages by votes (ascending - least to most voted)
        aur_packages.sort_by(|a, b| a.num_votes.cmp(&b.num_votes));

        // Build combined package list
        let mut all_packages = Vec::new();

        // Count official packages
        let mut official_count = 0;
        for line in official_packages.iter().filter(|line| !line.starts_with(char::is_whitespace)) {
            if line.find('[').is_some() {
                official_count += 1;
            }
        }

        // Add AUR packages (reversed index order)
        let total_packages = aur_packages.len() + official_count;
        for (i, pkg) in aur_packages.iter().enumerate() {
            let index = total_packages - i;
            all_packages.push((true, pkg.name.clone(), pkg.version.clone(), index));
        }

        // Add official packages
        let mut curr_index = official_count;
        for line in official_packages.iter().filter(|line| !line.starts_with(char::is_whitespace)) {
            if let Some(repo_start) = line.find('[') {
                let parts: Vec<&str> = line[..repo_start].trim().split_whitespace().collect();
                if parts.len() >= 1 {
                    let name = parts[0].to_string();
                    let version = parts.get(1).map(|&v| v.to_string()).unwrap_or_default();
                    all_packages.push((false, name, version, curr_index));
                    curr_index -= 1;
                }
            }
        }

        // Display packages
        println!("Found {} package(s):", all_packages.len());
        for (is_aur, name, version, index) in &all_packages {
            let source = if *is_aur { "AUR" } else { "repo" };
            let installed = if pacman::is_installed(name) { " (Installed)" } else { "" };
            println!("{}. {} ({}) [{}]{}", index, name, version, source, installed);
        }

        // Get user selection
        println!("\nEnter the package number to install (or 'back' to cancel):");
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if input.eq_ignore_ascii_case("back") {
            return Ok(());
        }

        let selection: usize = input.parse().map_err(|_| "Invalid selection")?;

        // Find the package with the matching index
        let selected_package = all_packages.iter()
            .find(|(_, _, _, idx)| *idx == selection)
            .ok_or_else(|| format!("Invalid package number: {}", selection))?;

        let (is_aur, name, _, _) = selected_package;
        println!("Installing {}...", name);

        // Install package
        if *is_aur {
            // Handle dependencies for AUR packages
            handle_dependencies(client, name).await?;

            // Clone and build
            let package_dir = aur::clone_package_repo(name).await?;
            let status = TokioCommand::new("makepkg")
                .args(["-si"])
                .current_dir(&package_dir)
                .status()
                .await?;

            if !status.success() {
                return Err(format!("Failed to install {}", name).into());
            }
        } else {
            // Install from official repos
            let status = Command::new("sudo")
                .args(["pacman", "-S", name])
                .status()?;

            if !status.success() {
                return Err(format!("Failed to install {}", name).into());
            }
        }

        println!("Package {} installed successfully.", name);
        Ok(())
    }

    pub async fn update_packages(client: &Client) -> Result<()> {
        // Get installed AUR packages
        let packages = pacman::get_installed_aur_packages()?;

        if !packages.is_empty() {
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

            // Process results and find updates
            for result in results {
                if let Ok(response) = result {
                    if let Some(aur_packages) = response.results {
                        for aur_pkg in aur_packages {
                            if let Some((_, local_ver)) = packages.iter()
                                .find(|(name, _)| name == &aur_pkg.name)
                            {
                                if let (Some(v_local), Some(v_aur)) =
                                    (Version::from(local_ver), Version::from(&aur_pkg.version)) {
                                    if v_local < v_aur {
                                        updates_available.push((
                                            aur_pkg.name,
                                            local_ver.clone(),
                                            aur_pkg.version
                                        ));
                                    }
                                }
                            }
                        }
                    }
                }
            }

            if updates_available.is_empty() {
                println!("No updates available for AUR packages.");
            } else {
                // Display available updates
                println!("\nUpdates available for {} package(s):", updates_available.len());
                for (i, (pkg, current, new)) in updates_available.iter().enumerate() {
                    println!("{}. {} ({} → {})", i + 1, pkg, current, new);
                }

                // Get user selection
                println!("\nEnter package numbers to update (e.g., '1 2 3'),");
                println!("press Enter to update all, or type 'back' to cancel:");
                let mut input = String::new();
                io::stdin().read_line(&mut input)?;
                let input = input.trim();

                if input.eq_ignore_ascii_case("back") {
                    return Ok(());
                }

                // Determine which packages to update
                let to_update = if input.is_empty() {
                    updates_available
                } else {
                    input.split_whitespace()
                        .filter_map(|s| s.parse::<usize>().ok())
                        .filter(|&n| n > 0 && n <= updates_available.len())
                        .map(|i| updates_available[i - 1].clone())
                        .collect()
                };

                // Update selected packages
                for (package, _, _) in to_update {
                    println!("\nUpdating {}...", package);
                    let pkg_path = aur::clone_package_repo(&package).await?;

                    let status = TokioCommand::new("makepkg")
                        .args(["-si", "--noconfirm"])
                        .current_dir(&pkg_path)
                        .status()
                        .await?;

                    if status.success() {
                        println!("{} updated successfully", package);
                    } else {
                        eprintln!("Failed to update {}", package);
                    }
                }
            }
        } else {
            println!("No AUR packages installed.");
        }

        // Update official packages
        println!("\nUpdating official packages via pacman...");
        let status = Command::new("sudo").args(["pacman", "-Syu"]).status()?;

        if status.success() {
            println!("Official packages updated successfully.");
        } else {
            eprintln!("Failed to update official packages.");
        }

        Ok(())
    }

    pub fn uninstall_package(package: &str) -> Result<()> {
        if !pacman::is_installed(package) {
            return Err(format!("Package {} is not installed", package).into());
        }

        let status = Command::new("sudo")
            .args(["pacman", "-Rns", package])
            .status()?;

        if status.success() {
            println!("Package {} removed successfully", package);
            Ok(())
        } else {
            Err(format!("Failed to remove package {}", package).into())
        }
    }
}

#[tokio::main]
async fn main() -> std::result::Result<(), Box<dyn StdError>> {
    println!("Welcome to aurorus!");
    println!("Type 'help' for a list of commands.\n");

    let client = Client::new();
    // Removed unused 'commands' variable

    loop {
        // Read user input
        print!("aurorus> ");
        io::stdout().flush()?;
        let mut input = String::new();
        io::stdin().read_line(&mut input)?;
        let input = input.trim();

        if input.is_empty() {
            continue;
        }

        // Parse command and arguments
        let mut parts = input.split_whitespace();
        let command = parts.next().unwrap().to_lowercase();
        let args: Vec<&str> = parts.collect();

        // Execute command
        match command.as_str() {
            "exit" => break,

            "help" => display::print_help(),

            "search" | "s" => {
                if args.is_empty() {
                    println!("Usage: search <package> or s <package>");
                    continue;
                }
                let query = args.join(" ");
                if let Err(e) = actions::search_packages(&client, &query).await {
                    eprintln!("Error: {}", e);
                }
            },

            "install" | "i" => {
                if args.is_empty() {
                    println!("Usage: install <package> or i <package>");
                    continue;
                }
                let query = args.join(" ");
                if let Err(e) = actions::install_package(&client, &query).await {
                    eprintln!("Error: {}", e);
                }
            },

            "uninstall" | "ui" => {
                if args.is_empty() {
                    println!("Usage: uninstall <package> or ui <package>");
                    continue;
                }
                let package = args.join(" ");
                if let Err(e) = actions::uninstall_package(&package) {
                    eprintln!("Error: {}", e);
                }
            },

            "update" | "up" => {
                if let Err(e) = actions::update_packages(&client).await {
                    eprintln!("Error: {}", e);
                }
            },

            _ => println!("Unknown command. Type 'help' to see available commands."),
        }
    }

    println!("Exiting aurorus. Goodbye!");
    Ok(())
}