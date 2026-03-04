use gtfs_realtime::FeedMessage;
use prost::Message;
use anyhow::{Result, Context};
use chrono::{Local, TimeZone};
use clap::{Parser, Subcommand};
use prettytable::{Table, Row, Cell};
use prettytable::format;
use serde::Deserialize;
use fuzzy_matcher::FuzzyMatcher;
use fuzzy_matcher::skim::SkimMatcherV2;
use dialoguer::{Select, Input, Confirm};
use indicatif::{ProgressBar, ProgressStyle};
use std::collections::HashMap;
use std::fs::File;
use std::path::PathBuf;

#[derive(Debug, Deserialize, Clone)]
struct Stop {
    stop_id: String,
    stop_name: String,
    stop_lat: f64,
    stop_lon: f64,
    location_type: Option<i32>,
    parent_station: Option<String>,
}

#[derive(Debug, Clone)]
struct Station {
    id: String,
    name: String,
    #[allow(dead_code)]
    lat: f64,
    #[allow(dead_code)]
    lon: f64,
    platforms: Vec<Platform>,
    lines: Vec<String>,  // Add this to track which subway lines serve this station
}

#[derive(Debug, Clone)]
struct Platform {
    id: String,
    name: String,
    direction: Option<String>,
}

#[derive(Debug, Clone)]
struct StationInfo {
    id: String,
    name: String,
    platform_count: usize,
    lines: Vec<String>,  // Add this
}

#[derive(Debug)]
struct Arrival {
    route: String,
    #[allow(dead_code)]
    trip_id: String,
    #[allow(dead_code)]
    stop_id: String,
    arrival_time: u64,
    #[allow(dead_code)]
    departure_time: Option<u64>,
    // headsign: Option<String>,
}

/// NYC MTA Subway Arrival CLI with interactive station selection
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Cli {
    #[command(subcommand)]
    command: Commands,
    
    /// Path to GTFS directory (default: ./gtfs_subway)
    #[arg(short, long, default_value = "./gtfs_subway")]
    gtfs_path: PathBuf,
}

#[derive(Subcommand)]
enum Commands {
    /// Search for stations by name
    Search {
        /// Station name to search for (partial matches allowed)
        name: String,
        
        /// Number of results to show
        #[arg(short, long, default_value_t = 10)]
        limit: usize,
    },
    
    /// Show arrivals for a station (interactive)
    Arrivals {
        /// Station name (if not provided, will prompt interactively)
        station: Option<String>,
        
        /// Specific platform ID (if known)
        #[arg(short, long)]
        platform: Option<String>,
        
        /// MTA Feed URL (auto-detected if not specified)
        #[arg(short, long)]
        feed: Option<String>,
        
        /// Number of upcoming trains to show
        #[arg(short, long, default_value_t = 10)]
        count: usize,
        
        /// Non-interactive mode (must provide station)
        #[arg(short, long)]
        yes: bool,
    },
    
    /// Interactive mode with menus
    Interactive,
}

#[derive(Debug)]
enum PlatformOrStop<'a> {
    Platform(&'a Platform),
    Stop(&'a Stop),
}

impl<'a> PlatformOrStop<'a> {
    fn id(&self) -> &str {
        match self {
            PlatformOrStop::Platform(p) => &p.id,
            PlatformOrStop::Stop(s) => &s.stop_id,
        }
    }
    
    fn name(&self) -> &str {
        match self {
            PlatformOrStop::Platform(p) => &p.name,
            PlatformOrStop::Stop(s) => &s.stop_name,
        }
    }
}

fn load_stops(gtfs_path: &PathBuf) -> Result<Vec<Stop>> {
    let stops_path = gtfs_path.join("stops.csv");
    let file = File::open(&stops_path)
        .with_context(|| format!("Failed to open stops.csv at {:?}", stops_path))?;
    
    let mut reader = csv::Reader::from_reader(file);
    let mut stops = Vec::new();
    
    for result in reader.deserialize() {
        let stop: Stop = result?;
        stops.push(stop);
    }
    
    Ok(stops)
}

fn build_station_index(stops: &[Stop]) -> Vec<Station> {
    let mut station_map: HashMap<String, Station> = HashMap::new();
    
    // First pass: collect all stations (location_type = 1)
    for stop in stops {
        if stop.location_type == Some(1) {
            station_map.insert(stop.stop_id.clone(), Station {
                id: stop.stop_id.clone(),
                name: stop.stop_name.clone(),
                lat: stop.stop_lat,
                lon: stop.stop_lon,
                platforms: Vec::new(),
                lines: Vec::new(),  // Initialize empty lines vector
            });
        }
    }
    
    // Second pass: add platforms to their parent stations and collect line info
    for stop in stops {
        if stop.location_type != Some(1) {
            if let Some(parent_id) = &stop.parent_station {
                if let Some(station) = station_map.get_mut(parent_id) {
                    // Try to infer direction from platform name or ID
                    let direction = if stop.stop_id.ends_with('N') {
                        Some("Northbound".to_string())
                    } else if stop.stop_id.ends_with('S') {
                        Some("Southbound".to_string())
                    } else {
                        None
                    };
                    
                    station.platforms.push(Platform {
                        id: stop.stop_id.clone(),
                        name: stop.stop_name.clone(),
                        direction,
                    });
                    
                    // Extract line from stop_id (first character often indicates line)
                    // This is a simplified approach - you might need a more sophisticated mapping
                    if let Some(first_char) = stop.stop_id.chars().next() {
                        let line = match first_char {
                            '1' | '2' | '3' | '4' | '5' | '6' | '7' => format!("{}", first_char),
                            'A' | 'C' | 'E' => format!("{}", first_char),
                            'B' | 'D' | 'F' | 'M' => format!("{}", first_char),
                            'G' => "G".to_string(),
                            'J' | 'Z' => format!("{}", first_char),
                            'L' => "L".to_string(),
                            'N' | 'Q' | 'R' | 'W' => format!("{}", first_char),
                            'S' => "S".to_string(),
                            _ => "?".to_string(),  // Question mark clearly shows it's unknown
                        };
                        
                        if !station.lines.contains(&line) {
                            station.lines.push(line);
                        }
                    }
                }
            }
        }
    }
    
    // Sort lines for each station
    for station in station_map.values_mut() {
        station.lines.sort();
    }
    
    station_map.into_values().collect()
}

fn search_stations(stations: &[Station], query: &str, limit: usize) -> Vec<StationInfo> {
    let matcher = SkimMatcherV2::default();
    let mut scored: Vec<(i64, StationInfo)> = stations
        .iter()
        .filter_map(|s| {
            matcher.fuzzy_match(&s.name.to_lowercase(), &query.to_lowercase())
                .map(|score| (score, StationInfo {
                    id: s.id.clone(),
                    name: s.name.clone(),
                    platform_count: s.platforms.len(),
                    lines: s.lines.clone(),  // Include lines
                }))
        })
        .collect();
    
    scored.sort_by(|a, b| b.0.cmp(&a.0));
    scored.into_iter().take(limit).map(|(_, info)| info).collect()
}

fn get_feed_for_station(station_id: &str) -> &'static str {
    match &station_id[..1] {
        "1" | "2" | "3" | "4" | "5" | "6" | "7" => "https://api-endpoint.mta.info/Dataservice/mtagtfsfeeds/nyct%2Fgtfs",
        "A" | "C" | "E" => "https://api-endpoint.mta.info/Dataservice/mtagtfsfeeds/nyct%2Fgtfs-ace",
        "B" | "D" | "F" | "M" => "https://api-endpoint.mta.info/Dataservice/mtagtfsfeeds/nyct%2Fgtfs-bdfm",
        "G" => "https://api-endpoint.mta.info/Dataservice/mtagtfsfeeds/nyct%2Fgtfs-g",
        "J" | "Z" => "https://api-endpoint.mta.info/Dataservice/mtagtfsfeeds/nyct%2Fgtfs-jz",
        "L" => "https://api-endpoint.mta.info/Dataservice/mtagtfsfeeds/nyct%2Fgtfs-l",
        "N" | "Q" | "R" | "W" => "https://api-endpoint.mta.info/Dataservice/mtagtfsfeeds/nyct%2Fgtfs-nqrw",
        "S" => "https://api-endpoint.mta.info/Dataservice/mtagtfsfeeds/nyct%2Fgtfs",
        _ => "https://api-endpoint.mta.info/Dataservice/mtagtfsfeeds/nyct%2Fgtfs-nqrw",
    }
}

fn fetch_arrivals(feed_url: &str, platform_id: &str, count: usize) -> Result<Vec<Arrival>> {
    let spinner = ProgressBar::new_spinner();
    spinner.set_style(ProgressStyle::default_spinner()
        .tick_strings(&["🚇", "🚆", "🚈", "🚊"]));
    spinner.set_message("Fetching MTA data...");
    
    let response = reqwest::blocking::get(feed_url)?;
    let bytes = response.bytes()?;
    let feed = FeedMessage::decode(bytes.as_ref())?;
    
    spinner.set_message("Processing arrivals...");
    
    let mut arrivals = Vec::new();
    
    for entity in &feed.entity {
        if let Some(trip_update) = &entity.trip_update {
            for stop_update in &trip_update.stop_time_update {
                if stop_update.stop_id == Some(platform_id.to_string()) {
                    if let Some(arrival) = &stop_update.arrival {
                        if let Some(arrival_time) = arrival.time {
                            // FIXED: Added & here to borrow instead of move
                            let trip = &trip_update.trip;

                            // Then handle the optional fields inside it
                            arrivals.push(Arrival {
                                route: trip.route_id.clone().unwrap_or_else(|| "Unknown".to_string()),
                                trip_id: trip.trip_id.clone().unwrap_or_else(|| "Unknown".to_string()),
                                stop_id: platform_id.to_string(),
                                arrival_time: arrival_time as u64,
                                departure_time: stop_update.departure.as_ref().and_then(|d| d.time).map(|t| t as u64),
                                // headsign: trip.trip_headsign.clone(),
                            });
                        }
                    }
                }
            }
        }
    }
    
    arrivals.sort_by(|a, b| a.arrival_time.cmp(&b.arrival_time));
    arrivals.truncate(count);
    
    spinner.finish_with_message("✅ Done!");
    
    Ok(arrivals)
}

fn display_arrivals(arrivals: &[Arrival], station_name: &str, platform_name: &str) {
    let now = Local::now().timestamp() as u64;
    
    println!("\n🚉 {} - {}", station_name, platform_name);
    println!("{}", "=".repeat(50));
    
    if arrivals.is_empty() {
        println!("No upcoming trains found for this platform.");
        return;
    }
    
    let mut table = Table::new();
    table.set_format(*format::consts::FORMAT_BOX_CHARS);
    table.set_titles(Row::new(vec![
        Cell::new("Route"),
        Cell::new("Arrival"),
        // Cell::new("Headsign"),
    ]));
    
    for arrival in arrivals {
        let datetime = Local.timestamp_opt(arrival.arrival_time as i64, 0)
            .single()
            .unwrap();
        let time_str = datetime.format("%I:%M:%S %p").to_string();
        
        let minutes_until = if arrival.arrival_time > now {
            (arrival.arrival_time - now) / 60
        } else {
            0
        };
        
        let arrival_display = format!("{} ({} min)", time_str, minutes_until);
        
        table.add_row(Row::new(vec![
            Cell::new(&arrival.route),
            Cell::new(&arrival_display),
            // Cell::new(arrival.headsign.as_deref().unwrap_or("--")),
        ]));
    }
    
    table.printstd();
}

fn interactive_mode(gtfs_path: PathBuf) -> Result<()> {
    println!("🚇 MTA Subway Arrival Tracker - Interactive Mode");
    println!("{}", "=".repeat(50));
    
    // Load stations
    let spinner = ProgressBar::new_spinner();
    spinner.set_message("Loading station data...");
    let stops = load_stops(&gtfs_path)?;
    let stations = build_station_index(&stops);
    spinner.finish_with_message(format!("✅ Loaded {} stations", stations.len()));
    
    loop {
        println!("\n📋 Main Menu");
        let options = vec![
            "Search for a station",
            // "Browse stations by line",
            "Exit",
        ];
        
        let selection = Select::new()
            .items(&options)
            .default(0)
            .interact()?;
        
        match selection {
            0 => {
                // Search for station
                let query: String = Input::new()
                    .with_prompt("Enter station name (partial name ok)")
                    .interact_text()?;
                
                let matches = search_stations(&stations, &query, 10);
                
                if matches.is_empty() {
                    println!("❌ No stations found matching '{}'", query);
                    continue;
                }
                
                // In interactive_mode, when showing search results:
                let station_names: Vec<String> = matches.iter()
                    .map(|s| {
                        let lines_display = if s.lines.is_empty() {
                            "".to_string()
                        } else {
                            format!(" [{}]", s.lines.join(", "))
                        };
                        format!("{}{} ({} platforms)", s.name, lines_display, s.platform_count)
                    })
                    .collect();
                
                let station_idx = Select::new()
                    .with_prompt("Select a station")
                    .items(&station_names)
                    .default(0)
                    .interact()?;
                
                let selected_info = &matches[station_idx];
                
                // Find the full station data
                let station = stations.iter()
                    .find(|s| s.id == selected_info.id)
                    .unwrap();
                
                if station.platforms.is_empty() {
                    println!("❌ No platforms found for this station");
                    continue;
                }
                
                // Select platform
                let platform_names: Vec<String> = station.platforms.iter()
                    .map(|p| {
                        if let Some(dir) = &p.direction {
                            format!("{} - {}", p.name, dir)
                        } else {
                            p.name.clone()
                        }
                    })
                    .collect();
                
                let platform_idx = Select::new()
                    .with_prompt("Select platform/direction")
                    .items(&platform_names)
                    .default(0)
                    .interact()?;
                
                let platform = &station.platforms[platform_idx];
                
                // Get feed and fetch arrivals
                let feed_url = get_feed_for_station(&platform.id);
                
                match fetch_arrivals(feed_url, &platform.id, 10) {
                    Ok(arrivals) => {
                        display_arrivals(&arrivals, &station.name, &platform.name);
                    }
                    Err(e) => {
                        println!("❌ Error fetching arrivals: {}", e);
                    }
                }
            }
            // 1 => {
            //     // Browse all stations
            //     println!("📚 All stations (sorted by name):");
            //     let mut all_stations: Vec<&Station> = stations.iter().collect();
            //     all_stations.sort_by(|a, b| a.name.cmp(&b.name));
                
            //     let station_names: Vec<String> = all_stations.iter()
            //         .take(20)
            //         .map(|s| format!("{} ({} platforms)", s.name, s.platforms.len()))
            //         .collect();
                
            //     if station_names.is_empty() {
            //         println!("No stations to display");
            //         continue;
            //     }
                
            //     let station_idx = Select::new()
            //         .with_prompt("Select a station (showing first 20)")
            //         .items(&station_names)
            //         .interact_opt()?;
                
            //     if let Some(idx) = station_idx {
            //         let station = all_stations[idx];
                    
            //         if station.platforms.is_empty() {
            //             println!("❌ No platforms found for this station");
            //             continue;
            //         }
                    
            //         let platform_names: Vec<String> = station.platforms.iter()
            //             .map(|p| {
            //                 if let Some(dir) = &p.direction {
            //                     format!("{} - {}", p.name, dir)
            //                 } else {
            //                     p.name.clone()
            //                 }
            //             })
            //             .collect();
                    
            //         let platform_idx = Select::new()
            //             .with_prompt("Select platform/direction")
            //             .items(&platform_names)
            //             .default(0)
            //             .interact()?;
                    
            //         let platform = &station.platforms[platform_idx];
                    
            //         let feed_url = get_feed_for_station(&platform.id);
                    
            //         match fetch_arrivals(feed_url, &platform.id, 10) {
            //             Ok(arrivals) => {
            //                 display_arrivals(&arrivals, &station.name, &platform.name);
            //             }
            //             Err(e) => {
            //                 println!("❌ Error fetching arrivals: {}", e);
            //             }
            //         }
            //     }
            // }
            1 => {
                println!("👋 Goodbye!");
                break;
            }
            _ => unreachable!(),
        }
        
        if !Confirm::new()
            .with_prompt("Do you want to check another station?")
            .default(true)
            .interact()?
        {
            break;
        }
    }
    
    Ok(())
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    
    match &cli.command {
        Commands::Search { name, limit } => {
            let stops = load_stops(&cli.gtfs_path)?;
            let stations = build_station_index(&stops);
            let matches = search_stations(&stations, name, *limit);
            
            if matches.is_empty() {
                println!("❌ No stations found matching '{}'", name);
                return Ok(());
            }
            
            println!("📋 Found {} matching stations:", matches.len());
            for (i, station) in matches.iter().enumerate() {
                println!("{}. {} ({} platforms)", i + 1, station.name, station.platform_count);
            }
        }
        
        Commands::Arrivals { station, platform, feed, count, yes } => {
            let stops = load_stops(&cli.gtfs_path)?;
            let stations = build_station_index(&stops);
            
            let (selected_station, selected_platform) = if let Some(platform_id) = platform {
                // Direct platform ID provided
                let platform_stop = stops.iter()
                    .find(|s| s.stop_id == *platform_id)
                    .with_context(|| format!("Platform ID '{}' not found", platform_id))?;
                
                let station = stations.iter()
                    .find(|s| s.id == *platform_stop.parent_station.as_ref().unwrap_or(&platform_stop.stop_id))
                    .context("Parent station not found")?;
                
                (station, PlatformOrStop::Stop(platform_stop))
            } else if let Some(station_name) = station {
                // Station name provided, need to select platform
                let matches = search_stations(&stations, station_name, 5);
                
                if matches.is_empty() {
                    anyhow::bail!("No stations found matching '{}'", station_name);
                }
                
                let selected_info = if matches.len() == 1 && *yes {
                    &matches[0]
                } else {
                    // In the Arrivals command handler, when showing station selection:
                    let station_names: Vec<String> = matches.iter()
                        .map(|s| {
                            let lines_display = if s.lines.is_empty() {
                                "".to_string()
                            } else {
                                format!(" [{}]", s.lines.join(", "))
                            };
                            format!("{}{} ({} platforms)", s.name, lines_display, s.platform_count)
                        })
                        .collect();
                    
                    let idx = Select::new()
                        .with_prompt("Select a station")
                        .items(&station_names)
                        .default(0)
                        .interact()?;
                    
                    &matches[idx]
                };
                
                // Find the full station
                let station = stations.iter()
                    .find(|s| s.id == selected_info.id)
                    .context("Station not found in full index")?;
                
                if station.platforms.is_empty() {
                    anyhow::bail!("No platforms found for station '{}'", station.name);
                }
                
                let platform = if station.platforms.len() == 1 && *yes {
                    PlatformOrStop::Platform(&station.platforms[0])
                } else {
                    let platform_names: Vec<String> = station.platforms.iter()
                        .map(|p| {
                            if let Some(dir) = &p.direction {
                                format!("{} - {}", p.name, dir)
                            } else {
                                p.name.clone()
                            }
                        })
                        .collect();
                    
                    let idx = Select::new()
                        .with_prompt("Select platform/direction")
                        .items(&platform_names)
                        .default(0)
                        .interact()?;
                    
                    PlatformOrStop::Platform(&station.platforms[idx])
                };
                
                (station, platform)
            } else {
                anyhow::bail!("Either --station or --platform must be provided in non-interactive mode");
            };
            
            let feed_url = feed.clone().unwrap_or_else(|| {
                get_feed_for_station(selected_platform.id()).to_string()
            });
            
            let arrivals = fetch_arrivals(&feed_url, selected_platform.id(), *count)?;
            
            display_arrivals(&arrivals, &selected_station.name, selected_platform.name());
        }
        
        Commands::Interactive => {
            interactive_mode(cli.gtfs_path)?;
        }
    }
    
    Ok(())
}