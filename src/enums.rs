use std::net::{IpAddr, Ipv4Addr};

use pnet::{
    packet::{
        arp::{ArpOperation, ArpOperations},
        icmp::IcmpType, icmpv6::Icmpv6Type,
    },
    util::MacAddr,
};
use strum::{Display, EnumCount, EnumIter, FromRepr};

#[derive(Debug, Clone, PartialEq)]
pub struct UDPPacketInfo {
    pub interface_name: String,
    pub source: IpAddr,
    pub source_port: u16,
    pub destination: IpAddr,
    pub destination_port: u16,
    pub length: u16,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TCPPacketInfo {
    pub interface_name: String,
    pub source: IpAddr,
    pub source_port: u16,
    pub destination: IpAddr,
    pub destination_port: u16,
    pub length: usize,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ARPPacketInfo {
    pub interface_name: String,
    pub source_mac: MacAddr,
    pub source_ip: Ipv4Addr,
    pub destination_mac: MacAddr,
    pub destination_ip: Ipv4Addr,
    pub operation: ArpOperation,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ICMPPacketInfo {
    pub interface_name: String,
    pub source: IpAddr,
    pub destination: IpAddr,
    pub seq: u16,
    pub id: u16,
    pub icmp_type: IcmpType,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ICMP6PacketInfo {
    pub interface_name: String,
    pub source: IpAddr,
    pub destination: IpAddr,
    pub icmp_type: Icmpv6Type,
}

#[derive(Debug, Clone, PartialEq)]
pub enum PacketsInfoTypesEnum {
    Arp(ARPPacketInfo),
    Tcp(TCPPacketInfo),
    Udp(UDPPacketInfo),
    Icmp(ICMPPacketInfo),
    Icmp6(ICMP6PacketInfo),
}

#[derive(Default, Clone, Copy, Display, FromRepr, EnumIter, EnumCount, PartialEq, Debug)]
pub enum TabsEnum {
    #[default]
    #[strum(to_string = "Discovery")]
    Discovery,
    #[strum(to_string = "Packets")]
    Packets,
    #[strum(to_string = "Ports")]
    Ports,
}

#[derive(Default, Clone, Copy, Display, FromRepr, EnumIter, EnumCount, PartialEq, Debug)]
pub enum PacketTypeEnum {
    #[default]
    #[strum(to_string = "All")]
    All,
    #[strum(to_string = "ARP")]
    Arp,
    #[strum(to_string = "TCP")]
    Tcp,
    #[strum(to_string = "UDP")]
    Udp,
    #[strum(to_string = "ICMP")]
    Icmp,
    #[strum(to_string = "ICMP6")]
    Icmp6,
}

impl PacketTypeEnum {
    pub fn previous(&self) -> Self {
        let current_index: usize = *self as usize;
        let previous_index = current_index.saturating_sub(1);
        Self::from_repr(previous_index).unwrap_or(*self)
    }

    pub fn next(&self) -> Self {
        let current_index = *self as usize;
        let next_index = current_index.saturating_add(1);
        Self::from_repr(next_index).unwrap_or(*self)
    }
}
