use chrono::{DateTime, Local};
use color_eyre::eyre::Result;
use csv::Writer;
use ratatui::{prelude::*, widgets::*};
use std::env;
use tokio::sync::mpsc::UnboundedSender;

use super::{discovery::ScannedIp, ports::ScannedIpPorts, Component, Frame};
use crate::{action::Action, enums::PacketsInfoTypesEnum};

#[derive(Default)]
pub struct Export {
    action_tx: Option<UnboundedSender<Action>>,
    home_dir: String,
}

impl Export {
    pub fn new() -> Self {
        Self {
            action_tx: None,
            home_dir: String::new(),
        }
    }

    fn get_user_home_dir(&mut self) {
        let mut home_dir = String::from("/root");
        if let Some(h_dir) = env::var_os("HOME") {
            home_dir = String::from(h_dir.to_str().unwrap());
        }
        if let Some(sudo_user) = env::var_os("SUDO_USER") {
            home_dir = format!("/home/{}", sudo_user.to_str().unwrap());
        }
        self.home_dir = format!("{}/.netscanner", home_dir);

        // -- create dot folder
        if std::fs::metadata(self.home_dir.clone()).is_err() {
            if std::fs::create_dir_all(self.home_dir.clone()).is_err() {
                println!("Failed to create export dir");
            }
        }
    }

    pub fn write_discovery(&mut self, data: Vec<ScannedIp>, timestamp: &String) -> Result<()> {
        let mut w = Writer::from_path(format!("{}/scanned_ips.{}.csv", self.home_dir, timestamp))?;

        // -- header
        w.write_record(["ip", "mac", "hostname", "vendor"])?;
        for s_ip in data {
            w.write_record([s_ip.ip, s_ip.mac, s_ip.hostname, s_ip.vendor])?;
        }
        w.flush()?;

        Ok(())
    }

    pub fn write_ports(&mut self, data: Vec<ScannedIpPorts>, timestamp: &String) -> Result<()> {
        let mut w =
            Writer::from_path(format!("{}/scanned_ports.{}.csv", self.home_dir, timestamp))?;

        // -- header
        w.write_record(["ip", "ports"])?;
        for s_ip in data {
            let ports: String = s_ip
                .ports
                .iter()
                .map(|n| n.to_string())
                .collect::<Vec<String>>()
                .join(":");
            w.write_record([s_ip.ip, ports])?;
        }
        w.flush()?;

        Ok(())
    }

    pub fn write_arp_packets(
        &mut self,
        data: Vec<(DateTime<Local>, PacketsInfoTypesEnum)>,
        timestamp: &String,
    ) -> Result<()> {
        let mut w = Writer::from_path(format!("{}/arp_packets.{}.csv", self.home_dir, timestamp))?;

        // -- header
        w.write_record(["time", "log"])?;
        for (t, p) in data {
            let mut log_str = String::from("");
            if let PacketsInfoTypesEnum::Arp(log) = p {
                log_str = log.raw_str;
            }
            w.write_record([t.to_string(), log_str])?;
        }
        w.flush()?;

        Ok(())
    }

    pub fn write_packets(
        &mut self,
        data: Vec<(DateTime<Local>, PacketsInfoTypesEnum)>,
        timestamp: &String,
        name: &str,
    ) -> Result<()> {
        let mut w = Writer::from_path(format!(
            "{}/{}_packets.{}.csv",
            self.home_dir, name, timestamp
        ))?;

        // -- header
        w.write_record(["time", "log"])?;
        for (t, p) in data {
            let log_str = match p {
                PacketsInfoTypesEnum::Icmp(log) => log.raw_str,
                PacketsInfoTypesEnum::Arp(log) => log.raw_str,
                PacketsInfoTypesEnum::Icmp6(log) => log.raw_str,
                PacketsInfoTypesEnum::Udp(log) => log.raw_str,
                PacketsInfoTypesEnum::Tcp(log) => log.raw_str,
            };
            w.write_record([t.to_string(), log_str])?;
        }
        w.flush()?;

        Ok(())
    }
}

impl Component for Export {
    fn init(&mut self, area: Rect) -> Result<()> {
        self.get_user_home_dir();
        Ok(())
    }

    fn register_action_handler(&mut self, tx: UnboundedSender<Action>) -> Result<()> {
        self.action_tx = Some(tx);
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        match action {
            Action::Export => {}
            Action::ExportData(data) => {
                let now = Local::now();
                // let now_str = now.format("%Y-%m-%d-%H-%M-%S").to_string();
                let now_str = now.timestamp().to_string();
                let _ = self.write_discovery(data.scanned_ips, &now_str);
                let _ = self.write_ports(data.scanned_ports, &now_str);
                let _ = self.write_packets(data.arp_packets, &now_str, "arp");
                let _ = self.write_packets(data.tcp_packets, &now_str, "tcp");
                let _ = self.write_packets(data.udp_packets, &now_str, "udp");
                let _ = self.write_packets(data.icmp_packets, &now_str, "icmp");
                let _ = self.write_packets(data.icmp6_packets, &now_str, "icmp6");
            }
            _ => {}
        }
        Ok(None)
    }

    fn draw(&mut self, f: &mut Frame<'_>, area: Rect) -> Result<()> {
        // let rect = Rect::new(0, 0, f.size().width, 1);
        // let version: &str = env!("CARGO_PKG_VERSION");
        // let title = format!(" Network Scanner (v{})", version);
        // f.render_widget(Paragraph::new(title), rect);
        Ok(())
    }
}
