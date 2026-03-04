use gtfs_realtime::FeedMessage;
use prost::Message;
use anyhow::Result;
use chrono::{Local, TimeZone};
use clap::Parser;
use prettytable::{Table, Row, Cell};
use prettytable::format;
// use std::collections::HashMap;

/// NYC MTA Subway Arrival Time CLI
#[derive(Parser)]
#[command(author, version, about, long_about = None)]
struct Args {
    /// GTFS Stop ID (e.g., "127N" for Times Square-42nd St northbound)
    #[arg(short, long)]
    stop_id: String,

    /// MTA Feed URL (defaults to N/Q/R/W)
    #[arg(short, long, default_value = "https://api-endpoint.mta.info/Dataservice/mtagtfsfeeds/nyct%2Fgtfs-nqrw")]
    feed_url: String,

    /// Number of upcoming trains to show
    #[arg(short, long, default_value_t = 10)]
    count: usize,

    /// Show all stops for each train (verbose)
    #[arg(short, long)]
    verbose: bool,
}

fn epoch_to_human(epoch_seconds: u64) -> String {
    let datetime = Local.timestamp_opt(epoch_seconds as i64, 0)
        .single()
        .unwrap();
    datetime.format("%I:%M:%S %p").to_string()
}

fn fetch_and_parse_subway_feed(feed_url: &str) -> Result<FeedMessage> {
    let response = reqwest::blocking::get(feed_url)?;
    let bytes = response.bytes()?;
    let feed = FeedMessage::decode(bytes.as_ref())?;
    Ok(feed)
}

#[derive(Debug)]
struct Arrival {
    route: String,
    trip_id: String,
    stop_id: String,
    arrival_time: u64,
    departure_time: Option<u64>,
    // headsign: Option<String>,
}

fn main() -> Result<()> {
    let args = Args::parse();
    
    println!("🚇 MTA Subway Arrival Tracker");
    println!("=================================");
    println!("Stop ID: {}", args.stop_id);
    println!("Feed: {}", args.feed_url);
    println!("Looking for {} upcoming trains...\n", args.count);
    
    // Fetch and parse the feed
    let feed = fetch_and_parse_subway_feed(&args.feed_url)?;
    
    // Collect all arrivals for the specified stop
    let mut arrivals: Vec<Arrival> = Vec::new();
    
    for entity in &feed.entity {
        if let Some(trip_update) = &entity.trip_update {
            for stop_update in &trip_update.stop_time_update {
                // Check if this stop update matches our target stop
                if stop_update.stop_id == Some(args.stop_id.clone()) {
                    if let Some(arrival) = &stop_update.arrival {
                        if let Some(arrival_time) = arrival.time {
                            let trip = &trip_update.trip; {
                                // Try to get headsign from trip
                                // let headsign = trip.trip_headsign.clone();
                                
                                arrivals.push(Arrival {
                                    route: trip.route_id.clone().unwrap_or_else(|| "Unknown".to_string()),
                                    trip_id: trip.trip_id.clone().unwrap_or_else(|| "Unknown".to_string()),
                                    stop_id: args.stop_id.clone(),
                                    arrival_time: arrival_time as u64,
                                    departure_time: stop_update.departure.as_ref().and_then(|d| d.time).map(|t| t as u64),
                                    // headsign,
                                });
                            }
                        }
                    }
                }
            }
        }
    }
    
    // Sort by arrival time
    arrivals.sort_by(|a, b| a.arrival_time.cmp(&b.arrival_time));
    
    // Take only the requested number
    let arrivals: Vec<_> = arrivals.into_iter().take(args.count).collect();
    
    if arrivals.is_empty() {
        println!("❌ No upcoming trains found for stop ID: {}", args.stop_id);
        println!("\n💡 Tip: Make sure you're using the correct GTFS stop ID format.");
        println!("   Example: '127N' for Times Square-42nd St northbound");
        println!("   Example: 'R03N' for 34th St-Herald Square northbound");
        return Ok(());
    }
    
    // Create a nice table
    let mut table = Table::new();
    table.set_format(*format::consts::FORMAT_BOX_CHARS);
    
    // Add header
    if args.verbose {
        table.set_titles(Row::new(vec![
            Cell::new("Route"),
            Cell::new("Arrival"),
            Cell::new("Departure"),
            Cell::new("Headsign"),
            Cell::new("Trip ID"),
        ]));
    } else {
        table.set_titles(Row::new(vec![
            Cell::new("Route"),
            Cell::new("Arrival"),
            Cell::new("Headsign"),
        ]));
    }
    
    // Add rows
    let now = Local::now().timestamp() as u64;
    
    for arrival in &arrivals {
        let arrival_str = epoch_to_human(arrival.arrival_time);
        
        // Calculate minutes until arrival
        let minutes_until = if arrival.arrival_time > now {
            (arrival.arrival_time - now) / 60
        } else {
            0
        };
        
        let arrival_display = format!("{} ({} min)", arrival_str, minutes_until);
        
        if args.verbose {
            let departure_str = arrival.departure_time
                .map(epoch_to_human)
                .unwrap_or_else(|| "--".to_string());
            
            table.add_row(Row::new(vec![
                Cell::new(&arrival.route),
                Cell::new(&arrival_display),
                Cell::new(&departure_str),
                // Cell::new(arrival.headsign.as_deref().unwrap_or("--")),
                Cell::new(&arrival.trip_id),
            ]));
        } else {
            table.add_row(Row::new(vec![
                Cell::new(&arrival.route),
                Cell::new(&arrival_display),
                // Cell::new(arrival.headsign.as_deref().unwrap_or("--")),
            ]));
        }
    }
    
    // Print the table
    table.printstd();
    
    // Show summary
    println!("\n📊 Found {} upcoming trains", arrivals.len());
    if let Some(first) = arrivals.first() {
        println!("🚆 First train: {} at {}", first.route, epoch_to_human(first.arrival_time));
    }
    
    Ok(())
}