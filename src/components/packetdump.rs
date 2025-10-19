use chrono::{DateTime, Local};
use color_eyre::eyre::Result;
use crossterm::event::{KeyCode, KeyEvent};

use pnet::datalink::{Channel, ChannelType, NetworkInterface};
use pnet::packet::icmpv6::Icmpv6Types;
use pnet::packet::{
    arp::ArpPacket,
    ethernet::{EtherTypes, EthernetPacket, MutableEthernetPacket},
    icmp::{echo_reply, echo_request, IcmpPacket, IcmpTypes},
    icmpv6::Icmpv6Packet,
    ip::{IpNextHeaderProtocol, IpNextHeaderProtocols},
    ipv4::Ipv4Packet,
    ipv6::Ipv6Packet,
    tcp::TcpPacket,
    udp::UdpPacket, Packet,
};
use pnet::util::MacAddr;

use ratatui::layout::Position;
use ratatui::style::Stylize;
use ratatui::{prelude::*, widgets::*};
use std::{
    net::{IpAddr, Ipv4Addr},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::{self, JoinHandle},
    time::Duration,
};
use tokio::sync::mpsc::Sender;
use tui_input::backend::crossterm::EventHandler;
use tui_input::Input;

use super::{Component, Frame};
use crate::{
    action::Action,
    config::DEFAULT_BORDER_STYLE,
    enums::{
        ARPPacketInfo, ICMP6PacketInfo, ICMPPacketInfo, PacketTypeEnum, PacketsInfoTypesEnum,
        TCPPacketInfo, TabsEnum, UDPPacketInfo,
    },
    layout::get_vertical_layout,
    mode::Mode,
    privilege,
    utils::MaxSizeVec,
};
use strum::{EnumCount, IntoEnumIterator};

const INPUT_SIZE: usize = 30;

// Network packet capture buffer size
// Standard Ethernet MTU is 1500 bytes + 14 bytes Ethernet header = 1514 bytes
// Jumbo frames can be up to 9000 bytes + headers = 9018 bytes
// We use 9100 to support jumbo frames with overhead for VLAN tags and extensions
const MAX_PACKET_BUFFER_SIZE: usize = 9100;

// Maximum number of packets to keep in history per packet type
// Limits memory usage to approximately 1000 packets * average packet size
// This provides sufficient history for analysis while preventing unbounded growth
const MAX_PACKET_HISTORY: usize = 1000;

#[derive(Debug, Clone, PartialEq)]
pub struct ArpPacketData {
    pub sender_mac: MacAddr,
    pub sender_ip: Ipv4Addr,
    pub target_mac: MacAddr,
    pub target_ip: Ipv4Addr,
}

pub struct PacketDump {
    active_tab: TabsEnum,
    action_tx: Option<Sender<Action>>,
    loop_thread: Option<JoinHandle<()>>,
    _should_quit: bool,
    dump_paused: Arc<AtomicBool>,
    dump_stop: Arc<AtomicBool>,
    active_interface: Option<NetworkInterface>,
    table_state: TableState,
    scrollbar_state: ScrollbarState,
    packet_type: PacketTypeEnum,
    input: Input,
    mode: Mode,
    filter_str: String,
    changed_interface: bool,

    arp_packets: MaxSizeVec<(DateTime<Local>, PacketsInfoTypesEnum)>,
    udp_packets: MaxSizeVec<(DateTime<Local>, PacketsInfoTypesEnum)>,
    tcp_packets: MaxSizeVec<(DateTime<Local>, PacketsInfoTypesEnum)>,
    icmp_packets: MaxSizeVec<(DateTime<Local>, PacketsInfoTypesEnum)>,
    icmp6_packets: MaxSizeVec<(DateTime<Local>, PacketsInfoTypesEnum)>,
    all_packets: MaxSizeVec<(DateTime<Local>, PacketsInfoTypesEnum)>,
}

impl Default for PacketDump {
    fn default() -> Self {
        Self::new()
    }
}

impl PacketDump {
    pub fn new() -> Self {
        Self {
            active_tab: TabsEnum::Discovery,
            action_tx: None,
            loop_thread: None,
            _should_quit: false,
            dump_paused: Arc::new(AtomicBool::new(false)),
            dump_stop: Arc::new(AtomicBool::new(false)),
            active_interface: None,
            table_state: TableState::default().with_selected(0),
            scrollbar_state: ScrollbarState::new(0),
            packet_type: PacketTypeEnum::All,
            input: Input::default().with_value(String::from("")),
            mode: Mode::Normal,
            filter_str: String::from(""),
            changed_interface: false,

            arp_packets: MaxSizeVec::new(MAX_PACKET_HISTORY),
            udp_packets: MaxSizeVec::new(MAX_PACKET_HISTORY),
            tcp_packets: MaxSizeVec::new(MAX_PACKET_HISTORY),
            icmp_packets: MaxSizeVec::new(MAX_PACKET_HISTORY),
            icmp6_packets: MaxSizeVec::new(MAX_PACKET_HISTORY),
            all_packets: MaxSizeVec::new(MAX_PACKET_HISTORY),
        }
    }

    fn handle_udp_packet(
        interface_name: &str,
        source: IpAddr,
        destination: IpAddr,
        packet: &[u8],
        action_tx: Sender<Action>,
    ) {
        let udp = UdpPacket::new(packet);
        if let Some(udp) = udp {
            let raw_str = format!(
                "[{}]: UDP Packet: {}:{} > {}:{}; length: {}",
                interface_name,
                source,
                udp.get_source(),
                destination,
                udp.get_destination(),
                udp.get_length()
            );

            let _ = action_tx.try_send(Action::PacketDump(
                Local::now(),
                PacketsInfoTypesEnum::Udp(UDPPacketInfo {
                    interface_name: interface_name.to_string(),
                    source,
                    source_port: udp.get_source(),
                    destination,
                    destination_port: udp.get_destination(),
                    length: udp.get_length() as usize,
                    raw_str,
                }),
                PacketTypeEnum::Udp,
            ));
        }
    }

    fn handle_icmp_packet(
        interface_name: &str,
        source: IpAddr,
        destination: IpAddr,
        packet: &[u8],
        action_tx: Sender<Action>,
    ) {
        let icmp_packet = IcmpPacket::new(packet);
        if let Some(icmp_packet) = icmp_packet {
            match icmp_packet.get_icmp_type() {
                IcmpTypes::EchoReply => {
                    // Validate packet can be parsed as echo reply
                    let Some(echo_reply_packet) = echo_reply::EchoReplyPacket::new(packet) else {
                        return;
                    };

                    let raw_str = format!(
                        "[{}]: ICMP echo reply {} -> {} (seq={:?}, id={:?})",
                        interface_name,
                        source,
                        destination,
                        echo_reply_packet.get_sequence_number(),
                        echo_reply_packet.get_identifier()
                    );

                    let _ = action_tx.try_send(Action::PacketDump(
                        Local::now(),
                        PacketsInfoTypesEnum::Icmp(ICMPPacketInfo {
                            interface_name: interface_name.to_string(),
                            source,
                            destination,
                            seq: echo_reply_packet.get_sequence_number(),
                            id: echo_reply_packet.get_identifier(),
                            icmp_type: IcmpTypes::EchoReply,
                            raw_str,
                        }),
                        PacketTypeEnum::Icmp,
                    ));
                }
                IcmpTypes::EchoRequest => {
                    // Validate packet can be parsed as echo request
                    let Some(echo_request_packet) = echo_request::EchoRequestPacket::new(packet) else {
                        return;
                    };

                    let raw_str = format!(
                        "[{}]: ICMP echo request {} -> {} (seq={:?}, id={:?})",
                        interface_name,
                        source,
                        destination,
                        echo_request_packet.get_sequence_number(),
                        echo_request_packet.get_identifier()
                    );

                    let _ = action_tx.try_send(Action::PacketDump(
                        Local::now(),
                        PacketsInfoTypesEnum::Icmp(ICMPPacketInfo {
                            interface_name: interface_name.to_string(),
                            source,
                            destination,
                            seq: echo_request_packet.get_sequence_number(),
                            id: echo_request_packet.get_identifier(),
                            icmp_type: IcmpTypes::EchoRequest,
                            raw_str,
                        }),
                        PacketTypeEnum::Icmp,
                    ));
                }
                _ => {}
            }
        }
    }

    fn handle_icmpv6_packet(
        interface_name: &str,
        source: IpAddr,
        destination: IpAddr,
        packet: &[u8],
        action_tx: Sender<Action>,
    ) {
        let icmpv6_packet = Icmpv6Packet::new(packet);
        if let Some(icmpv6_packet) = icmpv6_packet {
            let raw_str = format!(
                "[{}]: ICMPv6 packet {} -> {} (type={:?})",
                interface_name,
                source,
                destination,
                icmpv6_packet.get_icmpv6_type()
            );

            let _ = action_tx.try_send(Action::PacketDump(
                Local::now(),
                PacketsInfoTypesEnum::Icmp6(ICMP6PacketInfo {
                    interface_name: interface_name.to_string(),
                    source,
                    destination,
                    icmp_type: icmpv6_packet.get_icmpv6_type(),
                    raw_str,
                }),
                PacketTypeEnum::Icmp6,
            ));
        }
    }

    fn handle_tcp_packet(
        interface_name: &str,
        source: IpAddr,
        destination: IpAddr,
        packet: &[u8],
        action_tx: Sender<Action>,
    ) {
        let tcp = TcpPacket::new(packet);
        if let Some(tcp) = tcp {
            let raw_str = format!(
                "[{}]: TCP Packet: {}:{} > {}:{}; length: {}",
                interface_name,
                source,
                tcp.get_source(),
                destination,
                tcp.get_destination(),
                packet.len()
            );

            let _ = action_tx.try_send(Action::PacketDump(
                Local::now(),
                PacketsInfoTypesEnum::Tcp(TCPPacketInfo {
                    interface_name: interface_name.to_string(),
                    source,
                    source_port: tcp.get_source(),
                    destination,
                    destination_port: tcp.get_destination(),
                    length: packet.len(),
                    raw_str,
                }),
                PacketTypeEnum::Tcp,
            ));
        }
    }

    fn handle_transport_protocol(
        interface_name: &str,
        source: IpAddr,
        destination: IpAddr,
        protocol: IpNextHeaderProtocol,
        packet: &[u8],
        action_tx: Sender<Action>,
    ) {
        match protocol {
            IpNextHeaderProtocols::Udp => {
                Self::handle_udp_packet(interface_name, source, destination, packet, action_tx)
            }
            IpNextHeaderProtocols::Tcp => {
                Self::handle_tcp_packet(interface_name, source, destination, packet, action_tx)
            }
            IpNextHeaderProtocols::Icmp => {
                Self::handle_icmp_packet(interface_name, source, destination, packet, action_tx)
            }
            IpNextHeaderProtocols::Icmpv6 => {
                Self::handle_icmpv6_packet(interface_name, source, destination, packet, action_tx)
            }
            _ => {}
        }
    }

    fn handle_ipv4_packet(
        interface_name: &str,
        ethernet: &EthernetPacket,
        action_tx: Sender<Action>,
    ) {
        let header = Ipv4Packet::new(ethernet.payload());
        if let Some(header) = header {
            Self::handle_transport_protocol(
                interface_name,
                IpAddr::V4(header.get_source()),
                IpAddr::V4(header.get_destination()),
                header.get_next_level_protocol(),
                header.payload(),
                action_tx,
            );
        }
    }

    fn handle_ipv6_packet(
        interface_name: &str,
        ethernet: &EthernetPacket,
        action_tx: Sender<Action>,
    ) {
        let header = Ipv6Packet::new(ethernet.payload());
        if let Some(header) = header {
            Self::handle_transport_protocol(
                interface_name,
                IpAddr::V6(header.get_source()),
                IpAddr::V6(header.get_destination()),
                header.get_next_header(),
                header.payload(),
                action_tx,
            );
        } else {
            println!("[{}]: Malformed IPv6 Packet", interface_name);
        }
    }

    fn handle_arp_packet(
        interface_name: &str,
        ethernet: &EthernetPacket,
        action_tx: Sender<Action>,
    ) {
        let header = ArpPacket::new(ethernet.payload());
        if let Some(header) = header {
            let _ = action_tx.try_send(Action::ArpRecieve(ArpPacketData {
                sender_mac: header.get_sender_hw_addr(),
                sender_ip: header.get_sender_proto_addr(),
                target_mac: header.get_target_hw_addr(),
                target_ip: header.get_target_proto_addr(),
            }));

            let raw_str = format!(
                "[{}]: ARP packet: {}({}) > {}({}); operation: {:?}",
                interface_name,
                ethernet.get_source(),
                header.get_sender_proto_addr(),
                ethernet.get_destination(),
                header.get_target_proto_addr(),
                header.get_operation()
            );

            let _ = action_tx.try_send(Action::PacketDump(
                Local::now(),
                PacketsInfoTypesEnum::Arp(ARPPacketInfo {
                    interface_name: interface_name.to_string(),
                    source_mac: ethernet.get_source(),
                    source_ip: header.get_sender_proto_addr(),
                    destination_mac: ethernet.get_destination(),
                    destination_ip: header.get_target_proto_addr(),
                    operation: header.get_operation(),
                    raw_str,
                }),
                PacketTypeEnum::Arp,
            ));
        }
    }

    fn handle_ethernet_frame(
        interface: &NetworkInterface,
        ethernet: &EthernetPacket,
        action_tx: Sender<Action>,
    ) {
        let interface_name = &interface.name[..];
        match ethernet.get_ethertype() {
            EtherTypes::Ipv4 => Self::handle_ipv4_packet(interface_name, ethernet, action_tx),
            EtherTypes::Ipv6 => Self::handle_ipv6_packet(interface_name, ethernet, action_tx),
            EtherTypes::Arp => Self::handle_arp_packet(interface_name, ethernet, action_tx),
            _ => {}
        }
    }

    fn t_logic(action_tx: Sender<Action>, interface: NetworkInterface, stop: Arc<AtomicBool>) {
        // Configure optimized packet capture settings
        // Note: pnet does not support BPF filtering at the API level - all filtering
        // must be done in userspace after packets are captured. This is a known limitation
        // of the pnet library. For kernel-level filtering, consider using the pcap crate instead.
        let config = pnet::datalink::Config {
            // Increased buffer sizes for better performance with high packet rates
            // Larger buffers reduce syscall overhead and can handle burst traffic better
            write_buffer_size: 65536, // 64KB - sufficient for batch writes
            read_buffer_size: 65536,  // 64KB - can hold ~40-70 standard packets (MTU 1500)

            // Reduced read timeout for more responsive packet capture and faster shutdown
            // 100ms provides a good balance between CPU usage and responsiveness
            // This also ensures the stop signal is checked every 100ms maximum
            read_timeout: Some(Duration::from_millis(100)),

            write_timeout: None, // No write timeout needed for packet capture
            channel_type: ChannelType::Layer2, // Capture at Layer 2 (Ethernet)
            bpf_fd_attempts: 1000, // macOS/BSD: Try up to 1000 /dev/bpf* descriptors
            linux_fanout: None,    // Linux fanout not used for single-threaded capture
            promiscuous: true,     // Capture all packets on the interface, not just those addressed to this host
            socket_fd: None,       // Let pnet create its own socket
        };

        let (_, mut receiver) = match pnet::datalink::channel(&interface, config) {
            Ok(Channel::Ethernet(packet_tx, rx)) => (packet_tx, rx),
            Ok(_) => {
                let _ = action_tx.try_send(Action::Error(format!(
                    "Failed to create packet capture channel on interface '{}'.\n\
                    \n\
                    The network interface does not support the required Ethernet packet capture mode.\n\
                    This usually indicates:\n\
                    - Interface is not a standard Ethernet adapter (e.g., may be a tunnel, loopback, or wireless)\n\
                    - Interface does not support Layer 2 packet capture\n\
                    \n\
                    Please try selecting a different network interface.",
                    interface.name
                )));
                return;
            }
            Err(e) => {
                let error_msg = privilege::get_datalink_error_message(&e, &interface.name);
                let _ = action_tx.try_send(Action::Error(error_msg));
                return;
            }
        };

        loop {
            // Use SeqCst ordering to ensure we see the stop signal
            if stop.load(Ordering::SeqCst) {
                log::debug!("Packet capture thread received stop signal");
                break;
            }

            let mut buf: [u8; MAX_PACKET_BUFFER_SIZE] = [0u8; MAX_PACKET_BUFFER_SIZE];
            // Create mutable ethernet frame for handling special cases
            let Some(mut fake_ethernet_frame) = MutableEthernetPacket::new(&mut buf[..]) else {
                // Buffer too small, skip this iteration
                continue;
            };

            match receiver.next() {
                Ok(packet) => {
                    // Log warning if packet exceeds buffer size (indicates potential data loss)
                    if packet.len() > MAX_PACKET_BUFFER_SIZE {
                        log::warn!(
                            "Packet size ({} bytes) exceeds buffer capacity ({} bytes) on interface {}. \
                            Packet may be truncated.",
                            packet.len(),
                            MAX_PACKET_BUFFER_SIZE,
                            interface.name
                        );
                    }

                    let payload_offset;
                    if cfg!(any(target_os = "macos", target_os = "ios"))
                        && interface.is_up()
                        && !interface.is_broadcast()
                        && ((!interface.is_loopback() && interface.is_point_to_point())
                            || interface.is_loopback())
                    {
                        if interface.is_loopback() {
                            // The pnet code for BPF loopback adds a zero'd out Ethernet header
                            payload_offset = 14;
                        } else {
                            // Maybe is TUN interface
                            payload_offset = 0;
                        }
                        if packet.len() > payload_offset {
                            // Check if payload would exceed buffer after offset
                            let payload_size = packet.len() - payload_offset;
                            if payload_size > MAX_PACKET_BUFFER_SIZE - 14 {
                                log::warn!(
                                    "Payload size ({} bytes) after offset may exceed buffer on interface {}",
                                    payload_size,
                                    interface.name
                                );
                            }

                            // Try to parse as IPv4 packet to determine version
                            let version = match Ipv4Packet::new(&packet[payload_offset..]) {
                                Some(ipv4_packet) => ipv4_packet.get_version(),
                                None => continue, // Invalid packet, skip
                            };
                            if version == 4 {
                                fake_ethernet_frame.set_destination(MacAddr(0, 0, 0, 0, 0, 0));
                                fake_ethernet_frame.set_source(MacAddr(0, 0, 0, 0, 0, 0));
                                fake_ethernet_frame.set_ethertype(EtherTypes::Ipv4);
                                fake_ethernet_frame.set_payload(&packet[payload_offset..]);
                                Self::handle_ethernet_frame(
                                    &interface,
                                    &fake_ethernet_frame.to_immutable(),
                                    action_tx.clone(),
                                );
                                continue;
                            } else if version == 6 {
                                fake_ethernet_frame.set_destination(MacAddr(0, 0, 0, 0, 0, 0));
                                fake_ethernet_frame.set_source(MacAddr(0, 0, 0, 0, 0, 0));
                                fake_ethernet_frame.set_ethertype(EtherTypes::Ipv6);
                                fake_ethernet_frame.set_payload(&packet[payload_offset..]);
                                Self::handle_ethernet_frame(
                                    &interface,
                                    &fake_ethernet_frame.to_immutable(),
                                    action_tx.clone(),
                                );
                                continue;
                            }
                        }
                    }
                    // Parse ethernet packet - skip if invalid
                    if let Some(ethernet_packet) = EthernetPacket::new(packet) {
                        Self::handle_ethernet_frame(
                            &interface,
                            &ethernet_packet,
                            action_tx.clone(),
                        );
                    }
                }
                // Err(e) => println!("packetdump: unable to receive packet: {}", e),
                Err(_e) => {}
            }
        }
    }

    fn start_loop(&mut self) {
        if self.loop_thread.is_none() {
            // Require both action_tx and active_interface to start loop
            let Some(tx) = self.action_tx.clone() else {
                return;
            };
            let Some(interface) = self.active_interface.clone() else {
                return;
            };

            log::debug!("Starting packet capture thread for interface: {}", interface.name);
            let dump_stop = self.dump_stop.clone();
            let t_handle = thread::spawn(move || {
                Self::t_logic(tx, interface, dump_stop);
            });
            self.loop_thread = Some(t_handle);
        }
    }

    fn restart_loop(&mut self) {
        log::debug!("Requesting packet capture thread to stop");
        // Use SeqCst ordering for consistent memory visibility across threads
        self.dump_stop.store(true, Ordering::SeqCst);

        // Wait for thread to finish with timeout
        if let Some(handle) = self.loop_thread.take() {
            // Try to join the thread with a timeout
            // We use a simple timeout mechanism by checking if thread is finished
            let start = std::time::Instant::now();
            let timeout = Duration::from_secs(1);

            while !handle.is_finished() && start.elapsed() < timeout {
                thread::sleep(Duration::from_millis(50));
            }

            if handle.is_finished() {
                // Thread finished gracefully, join it to clean up
                match handle.join() {
                    Ok(_) => log::debug!("Packet capture thread stopped successfully"),
                    Err(_) => log::warn!("Packet capture thread panicked during shutdown"),
                }
            } else {
                // Thread didn't finish in time, but we've signaled it to stop
                // Store the handle back so Drop can handle it
                log::warn!("Packet capture thread did not stop within timeout, will be cleaned up on drop");
                self.loop_thread = Some(handle);
            }
        }
    }

    pub fn get_array_by_packet_type(
        &self,
        packet_type: PacketTypeEnum,
    ) -> &std::collections::VecDeque<(DateTime<Local>, PacketsInfoTypesEnum)> {
        match packet_type {
            PacketTypeEnum::Arp => self.arp_packets.get_deque(),
            PacketTypeEnum::Tcp => self.tcp_packets.get_deque(),
            PacketTypeEnum::Udp => self.udp_packets.get_deque(),
            PacketTypeEnum::Icmp => self.icmp_packets.get_deque(),
            PacketTypeEnum::Icmp6 => self.icmp6_packets.get_deque(),
            PacketTypeEnum::All => self.all_packets.get_deque(),
        }
    }

    pub fn get_arp_packages(&self) -> Vec<(DateTime<Local>, PacketsInfoTypesEnum)> {
        self.arp_packets.get_vec()
    }

    pub fn clone_array_by_packet_type(
        &self,
        packet_type: PacketTypeEnum,
    ) -> Vec<(DateTime<Local>, PacketsInfoTypesEnum)> {
        match packet_type {
            PacketTypeEnum::Arp => self.arp_packets.get_vec(),
            PacketTypeEnum::Tcp => self.tcp_packets.get_vec(),
            PacketTypeEnum::Udp => self.udp_packets.get_vec(),
            PacketTypeEnum::Icmp => self.icmp_packets.get_vec(),
            PacketTypeEnum::Icmp6 => self.icmp6_packets.get_vec(),
            PacketTypeEnum::All => self.all_packets.get_vec(),
        }
    }

    fn set_scrollbar_height(&mut self) {
        let logs_len = self.get_array_by_packet_type(self.packet_type).len();
        if logs_len > 0 {
            self.scrollbar_state = self.scrollbar_state.content_length(logs_len - 1);
        }
    }

    fn previous_in_table(&mut self) {
        let index = match self.table_state.selected() {
            Some(index) => {
                let logs = self.get_array_by_packet_type(self.packet_type);
                let logs_len = logs.len();
                if index == 0 {
                    if logs_len > 0 {
                        logs.len() - 1
                    } else {
                        0
                    }
                } else {
                    index - 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(index));
        self.scrollbar_state = self.scrollbar_state.position(index);
    }

    fn next_in_table(&mut self) {
        let index = match self.table_state.selected() {
            Some(index) => {
                let logs = self.get_array_by_packet_type(self.packet_type);
                if logs.is_empty() || index >= logs.len() - 1 {
                    0
                } else {
                    index + 1
                }
            }
            None => 0,
        };
        self.table_state.select(Some(index));
        self.scrollbar_state = self.scrollbar_state.position(index);
    }

    /// Formats an ICMP packet into styled spans for table display
    fn format_icmp_packet_row(icmp: &ICMPPacketInfo) -> Vec<Span<'static>> {
        let mut spans = vec![];

        spans.push(Span::styled(
            format!("[{}] ", icmp.interface_name.clone()),
            Style::default().fg(Color::Green),
        ));
        spans.push(Span::styled(
            "ICMP",
            Style::default().fg(Color::Black).bg(Color::White),
        ));

        match icmp.icmp_type {
            IcmpTypes::EchoRequest => {
                spans.push(Span::styled(
                    " echo request ",
                    Style::default().fg(Color::Yellow),
                ));
            }
            IcmpTypes::EchoReply => {
                spans.push(Span::styled(
                    " echo reply ",
                    Style::default().fg(Color::Yellow),
                ));
            }
            _ => {}
        }

        spans.push(Span::styled(
            icmp.source.to_string(),
            Style::default().fg(Color::Blue),
        ));
        spans.push(Span::styled(" -> ", Style::default().fg(Color::Yellow)));
        spans.push(Span::styled(
            icmp.destination.to_string(),
            Style::default().fg(Color::Blue),
        ));
        spans.push(Span::styled("(seq=", Style::default().fg(Color::Yellow)));
        spans.push(Span::styled(
            format!("{:?}", icmp.seq.to_string()),
            Style::default().fg(Color::Green),
        ));
        spans.push(Span::styled(", ", Style::default().fg(Color::Yellow)));
        spans.push(Span::styled("id=", Style::default().fg(Color::Yellow)));
        spans.push(Span::styled(
            format!("{:?}", icmp.id.to_string()),
            Style::default().fg(Color::Green),
        ));
        spans.push(Span::styled(")", Style::default().fg(Color::Yellow)));

        spans
    }

    /// Formats an ICMPv6 packet into styled spans for table display
    fn format_icmp6_packet_row(icmp: &ICMP6PacketInfo) -> Vec<Span<'static>> {
        let mut spans = vec![];

        spans.push(Span::styled(
            format!("[{}] ", icmp.interface_name.clone()),
            Style::default().fg(Color::Green),
        ));
        spans.push(Span::styled(
            "ICMP6",
            Style::default().fg(Color::Red).bg(Color::Black),
        ));

        let icmp_type_str = match icmp.icmp_type {
            Icmpv6Types::EchoRequest => " echo request ",
            Icmpv6Types::EchoReply => " echo reply ",
            Icmpv6Types::NeighborAdvert => " neighbor advert ",
            Icmpv6Types::NeighborSolicit => " neighbor solicit ",
            Icmpv6Types::Redirect => " redirect ",
            _ => " unknown ",
        };
        spans.push(Span::styled(
            icmp_type_str,
            Style::default().fg(Color::Yellow),
        ));

        spans.push(Span::styled(
            icmp.source.to_string(),
            Style::default().fg(Color::Blue),
        ));
        spans.push(Span::styled(" -> ", Style::default().fg(Color::Yellow)));
        spans.push(Span::styled(
            icmp.destination.to_string(),
            Style::default().fg(Color::Blue),
        ));
        spans.push(Span::styled(", ", Style::default().fg(Color::Yellow)));
        spans.push(Span::styled(")", Style::default().fg(Color::Yellow)));

        spans
    }

    /// Formats a UDP packet into styled spans for table display
    fn format_udp_packet_row(udp: &UDPPacketInfo) -> Vec<Span<'static>> {
        let mut spans = vec![];

        spans.push(Span::styled(
            format!("[{}] ", udp.interface_name.clone()),
            Style::default().fg(Color::Green),
        ));
        spans.push(Span::styled(
            "UDP",
            Style::default().fg(Color::Yellow).bg(Color::Blue),
        ));
        spans.push(Span::styled(
            " Packet: ",
            Style::default().fg(Color::Yellow),
        ));
        spans.push(Span::styled(
            udp.source.to_string(),
            Style::default().fg(Color::Blue),
        ));
        spans.push(Span::styled(":", Style::default().fg(Color::Yellow)));
        spans.push(Span::styled(
            udp.source_port.to_string(),
            Style::default().fg(Color::Green),
        ));
        spans.push(Span::styled(" > ", Style::default().fg(Color::Yellow)));
        spans.push(Span::styled(
            udp.destination.to_string(),
            Style::default().fg(Color::Blue),
        ));
        spans.push(Span::styled(":", Style::default().fg(Color::Yellow)));
        spans.push(Span::styled(
            udp.destination_port.to_string(),
            Style::default().fg(Color::Green),
        ));
        spans.push(Span::styled(";", Style::default().fg(Color::Yellow)));
        spans.push(Span::styled(
            " length: ",
            Style::default().fg(Color::Yellow),
        ));
        spans.push(Span::styled(
            format!("{}", udp.length),
            Style::default().fg(Color::Red),
        ));

        spans
    }

    /// Formats a TCP packet into styled spans for table display
    fn format_tcp_packet_row(tcp: &TCPPacketInfo) -> Vec<Span<'static>> {
        let mut spans = vec![];

        spans.push(Span::styled(
            format!("[{}] ", tcp.interface_name.clone()),
            Style::default().fg(Color::Green),
        ));
        spans.push(Span::styled(
            "TCP",
            Style::default().fg(Color::Black).bg(Color::Green),
        ));
        spans.push(Span::styled(
            " Packet: ",
            Style::default().fg(Color::Yellow),
        ));
        spans.push(Span::styled(
            tcp.source.to_string(),
            Style::default().fg(Color::Blue),
        ));
        spans.push(Span::styled(":", Style::default().fg(Color::Yellow)));
        spans.push(Span::styled(
            tcp.source_port.to_string(),
            Style::default().fg(Color::Green),
        ));
        spans.push(Span::styled(" > ", Style::default().fg(Color::Yellow)));
        spans.push(Span::styled(
            tcp.destination.to_string(),
            Style::default().fg(Color::Blue),
        ));
        spans.push(Span::styled(":", Style::default().fg(Color::Yellow)));
        spans.push(Span::styled(
            tcp.destination_port.to_string(),
            Style::default().fg(Color::Green),
        ));
        spans.push(Span::styled(";", Style::default().fg(Color::Yellow)));
        spans.push(Span::styled(
            " length: ",
            Style::default().fg(Color::Yellow),
        ));
        spans.push(Span::styled(
            format!("{}", tcp.length),
            Style::default().fg(Color::Red),
        ));

        spans
    }

    /// Formats an ARP packet into styled spans for table display
    fn format_arp_packet_row(arp: &ARPPacketInfo) -> Vec<Span<'static>> {
        let mut spans = vec![];

        spans.push(Span::styled(
            format!("[{}] ", arp.interface_name.clone()),
            Style::default().fg(Color::Green),
        ));
        spans.push(Span::styled(
            "ARP",
            Style::default().fg(Color::Yellow).bg(Color::Red),
        ));
        spans.push(Span::styled(
            " Packet: ",
            Style::default().fg(Color::Yellow),
        ));
        spans.push(Span::styled(
            arp.source_mac.to_string(),
            Style::default().fg(Color::Green),
        ));
        spans.push(Span::styled(
            arp.source_ip.to_string(),
            Style::default().fg(Color::Blue),
        ));
        spans.push(Span::styled(" > ", Style::default().fg(Color::Yellow)));
        spans.push(Span::styled(
            arp.destination_mac.to_string(),
            Style::default().fg(Color::Green),
        ));
        spans.push(Span::styled(
            arp.destination_ip.to_string(),
            Style::default().fg(Color::Blue),
        ));
        spans.push(Span::styled(";", Style::default().fg(Color::Yellow)));
        spans.push(Span::styled(
            format!(" {:?}", arp.operation),
            Style::default().fg(Color::Red),
        ));

        spans
    }

    /// Retrieves and filters packet data based on packet type and filter string,
    /// then formats each packet into a table row with styled spans
    fn get_table_rows_by_packet_type<'a>(&mut self, packet_type: PacketTypeEnum) -> Vec<Row<'a>> {
        let f_str = self.filter_str.clone();
        let logs_data = self.get_array_by_packet_type(packet_type);

        // Filter packets based on filter string
        let mut logs: Vec<(DateTime<Local>, PacketsInfoTypesEnum)> = vec![];
        for (d, p) in logs_data {
            let matches_filter = match p {
                PacketsInfoTypesEnum::Icmp(log) => log.raw_str.contains(f_str.as_str()),
                PacketsInfoTypesEnum::Arp(log) => log.raw_str.contains(f_str.as_str()),
                PacketsInfoTypesEnum::Icmp6(log) => log.raw_str.contains(f_str.as_str()),
                PacketsInfoTypesEnum::Udp(log) => log.raw_str.contains(f_str.as_str()),
                PacketsInfoTypesEnum::Tcp(log) => log.raw_str.contains(f_str.as_str()),
            };

            if matches_filter {
                logs.push((d.to_owned(), p.to_owned()));
            }
        }

        // Format each packet into a table row
        let rows: Vec<Row> = logs
            .iter()
            .map(|(time, log)| {
                let t = time.format("%H:%M:%S").to_string();

                let spans = match log {
                    PacketsInfoTypesEnum::Icmp(icmp) => Self::format_icmp_packet_row(icmp),
                    PacketsInfoTypesEnum::Icmp6(icmp6) => Self::format_icmp6_packet_row(icmp6),
                    PacketsInfoTypesEnum::Udp(udp) => Self::format_udp_packet_row(udp),
                    PacketsInfoTypesEnum::Tcp(tcp) => Self::format_tcp_packet_row(tcp),
                    PacketsInfoTypesEnum::Arp(arp) => Self::format_arp_packet_row(arp),
                };

                let line = Line::from(spans);
                Row::new(vec![
                    Cell::from(Span::styled(t, Style::default().fg(Color::Cyan))),
                    Cell::from(line),
                ])
            })
            .collect();
        rows
    }

    fn make_table(rows: Vec<Row>, packet_type: PacketTypeEnum, dump_paused: bool) -> Table {
        let header = Row::new(vec!["time", "packet log"])
            .style(Style::default().fg(Color::Yellow))
            .top_margin(1)
            .bottom_margin(1);

        let mut type_titles = vec![
            Span::styled("|", Style::default().fg(Color::Yellow)),
            Span::styled(
                String::from(char::from_u32(0x25c0).unwrap_or('<')),
                Style::default().fg(Color::Red),
            ),
        ];
        let mut enum_titles = PacketTypeEnum::iter()
            .enumerate()
            .map(|(idx, p)| {
                let mut span_str = format!("{} ", p);
                if idx == PacketTypeEnum::COUNT - 1 {
                    span_str = format!("{}", p);
                }
                if p == packet_type {
                    Span::styled(span_str, Style::new().green().bold())
                } else {
                    Span::styled(span_str, Style::new().dark_gray())
                }
            })
            .collect::<Vec<Span>>();
        type_titles.append(&mut enum_titles);
        type_titles.push(Span::styled(
            String::from(char::from_u32(0x25b6).unwrap_or('>')),
            Style::default().fg(Color::Red),
        ));
        type_titles.push(Span::styled("|", Style::default().fg(Color::Yellow)));

        // -- dump title
        let mut dump_spans = vec![
            Span::styled("|", Style::default().fg(Color::Yellow)),
            Span::styled(
                "d",
                Style::default().add_modifier(Modifier::BOLD).fg(Color::Red),
            ),
            Span::styled("ump:", Style::default().fg(Color::Yellow)),
        ];
        if dump_paused {
            dump_spans.push(Span::styled("paused", Style::default().fg(Color::DarkGray)))
        } else {
            dump_spans.push(Span::styled("running", Style::default().fg(Color::Green)))
        }
        dump_spans.push(Span::styled("|", Style::default().fg(Color::Yellow)));

        let table = Table::new(rows, [Constraint::Min(10), Constraint::Percentage(100)])
            .header(header)
            .block(
                Block::new()
                    .title(
                        ratatui::widgets::block::Title::from(Line::from(dump_spans))
                            .position(ratatui::widgets::block::Position::Top)
                            .alignment(Alignment::Right),
                    )
                    .title(
                        ratatui::widgets::block::Title::from(Line::from(vec![
                            Span::raw("|"),
                            Span::styled(
                                "e",
                                Style::default().add_modifier(Modifier::BOLD).fg(Color::Red),
                            ),
                            Span::styled("xport data", Style::default().fg(Color::Yellow)),
                            Span::raw("|"),
                        ]))
                        .alignment(Alignment::Left)
                        .position(ratatui::widgets::block::Position::Bottom),
                    )
                    .title(
                        ratatui::widgets::block::Title::from(Span::styled(
                            "|Packets|",
                            Style::default().fg(Color::Yellow),
                        ))
                        .position(ratatui::widgets::block::Position::Top)
                        .alignment(Alignment::Right),
                    )
                    .title(
                        ratatui::widgets::block::Title::from(Line::from(type_titles))
                            .position(ratatui::widgets::block::Position::Top)
                            .alignment(Alignment::Left),
                    )
                    .title(
                        ratatui::widgets::block::Title::from(Line::from(vec![
                            Span::styled("|", Style::default().fg(Color::Yellow)),
                            Span::styled(
                                String::from(char::from_u32(0x25b2).unwrap_or('>')),
                                Style::default().fg(Color::Red),
                            ),
                            Span::styled(
                                String::from(char::from_u32(0x25bc).unwrap_or('>')),
                                Style::default().fg(Color::Red),
                            ),
                            Span::styled("select|", Style::default().fg(Color::Yellow)),
                        ]))
                        .position(ratatui::widgets::block::Position::Bottom)
                        .alignment(Alignment::Right),
                    )
                    .border_style(Style::default().fg(Color::Rgb(100, 100, 100)))
                    .borders(Borders::ALL) // .padding(Padding::new(1, 0, 2, 0)),
                    .border_type(DEFAULT_BORDER_STYLE),
            )
            .highlight_symbol(Span::styled(
                String::from(char::from_u32(0x25b6).unwrap_or('>')),
                Style::default().fg(Color::Red),
            ))
            .column_spacing(1);
        table
    }

    pub fn make_scrollbar<'a>() -> Scrollbar<'a> {
        let scrollbar = Scrollbar::default()
            .orientation(ScrollbarOrientation::VerticalRight)
            .style(Style::default().fg(Color::Rgb(100, 100, 100)))
            .begin_symbol(None)
            .end_symbol(None);
        scrollbar
    }

    fn make_input(&self, scroll: usize) -> Paragraph<'_> {
        let input = Paragraph::new(self.input.value())
            .style(Style::default().fg(Color::Green))
            .scroll((0, scroll as u16))
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(match self.mode {
                        Mode::Input => Style::default().fg(Color::Green),
                        Mode::Normal => Style::default().fg(Color::Rgb(100, 100, 100)),
                    })
                    .border_type(DEFAULT_BORDER_STYLE)
                    .title(
                        ratatui::widgets::block::Title::from(Line::from(vec![
                            Span::raw("|"),
                            Span::styled(
                                "i",
                                Style::default().add_modifier(Modifier::BOLD).fg(Color::Red),
                            ),
                            Span::styled("nput", Style::default().fg(Color::Yellow)),
                            Span::raw("/"),
                            Span::styled(
                                "ESC",
                                Style::default().add_modifier(Modifier::BOLD).fg(Color::Red),
                            ),
                            Span::raw("|"),
                        ]))
                        .alignment(Alignment::Right)
                        .position(ratatui::widgets::block::Position::Bottom),
                    )
                    .title(
                        ratatui::widgets::block::Title::from(Line::from(vec![
                            Span::raw("|"),
                            Span::styled(
                                "c",
                                Style::default().add_modifier(Modifier::BOLD).fg(Color::Red),
                            ),
                            Span::styled("lear", Style::default().fg(Color::Yellow)),
                            Span::raw("|"),
                        ]))
                        .alignment(Alignment::Left)
                        .position(ratatui::widgets::block::Position::Bottom),
                    ),
            );
        input
    }

    fn set_filter_str(&mut self, value: String) {
        self.filter_str = value;
    }
}

impl Drop for PacketDump {
    fn drop(&mut self) {
        // Signal thread to stop
        self.dump_stop.store(true, Ordering::SeqCst);

        // Wait for thread to finish with timeout
        if let Some(handle) = self.loop_thread.take() {
            log::debug!("PacketDump dropping, waiting for thread to finish");
            let start = std::time::Instant::now();
            let timeout = Duration::from_secs(2);

            while !handle.is_finished() && start.elapsed() < timeout {
                thread::sleep(Duration::from_millis(50));
            }

            if handle.is_finished() {
                // Thread finished gracefully
                let _ = handle.join();
                log::debug!("PacketDump thread cleaned up successfully");
            } else {
                log::warn!("PacketDump thread did not finish within timeout during drop");
                // Thread handle will be dropped, potentially causing thread termination
            }
        }
    }
}

impl Component for PacketDump {
    fn register_action_handler(&mut self, action_tx: Sender<Action>) -> Result<()> {
        self.action_tx = Some(action_tx);
        Ok(())
    }

    fn as_any(&self) -> &dyn std::any::Any {
        self
    }

    fn handle_key_events(&mut self, key: KeyEvent) -> Result<Option<Action>> {
        if self.active_tab == TabsEnum::Packets {
            let action = match self.mode {
                Mode::Normal => return Ok(None),
                Mode::Input => match key.code {
                    KeyCode::Enter => {
                        if let Some(_sender) = &self.action_tx {
                            self.set_filter_str(self.input.value().to_string());
                            // self.set_cidr(self.input.value().to_string(), true);
                        }
                        Action::ModeChange(Mode::Normal)
                    }
                    _ => {
                        self.input.handle_event(&crossterm::event::Event::Key(key));
                        return Ok(None);
                    }
                },
            };
            Ok(Some(action))
        } else {
            Ok(None)
        }
    }

    fn update(&mut self, action: Action) -> Result<Option<Action>> {
        // -- change thread loop if interface is changed
        if self.changed_interface {
            if let Some(ref lt) = self.loop_thread {
                if lt.is_finished() {
                    // Thread has finished, clean it up and start new one
                    self.loop_thread = None;
                    self.dump_stop.store(false, Ordering::SeqCst);
                    log::debug!("Previous packet capture thread finished, starting new one");
                    self.start_loop();
                    self.changed_interface = false;
                }
            } else {
                // No thread running, safe to start immediately
                self.dump_stop.store(false, Ordering::SeqCst);
                self.start_loop();
                self.changed_interface = false;
            }
        }

        // -- tab change
        if let Action::TabChange(tab) = action {
            let _ = self.tab_changed(tab);
        }
        // -- active interface set
        if let Action::ActiveInterface(ref interface) = action {
            let mut was_none = false;
            if self.active_interface.is_none() {
                was_none = true;
            }
            self.active_interface = Some(interface.clone());
            if was_none {
                self.start_loop();
            } else {
                self.changed_interface = true;
                self.restart_loop();
            }
        }
        if self.active_tab == TabsEnum::Packets {
            // -- prev & next select item in table
            if let Action::Down = action {
                self.next_in_table();
            }
            if let Action::Up = action {
                self.previous_in_table();
            }
            if let Action::Left = action {
                self.packet_type = self.packet_type.previous();
                self.set_scrollbar_height();
                self.table_state.select(Some(0));
                self.set_scrollbar_height();
            }
            if let Action::Right = action {
                self.packet_type = self.packet_type.next();
                self.set_scrollbar_height();
                self.table_state.select(Some(0));
                self.set_scrollbar_height();
            }
            // -- dumping toggle
            if let Action::DumpToggle = action {
                if self.dump_paused.load(Ordering::Relaxed) {
                    self.dump_paused.store(false, Ordering::Relaxed);
                    self.start_loop();
                } else {
                    self.dump_paused.store(true, Ordering::Relaxed);
                    self.loop_thread = None;
                }
            }

            // -- MODE CHANGE
            if let Action::ModeChange(mode) = action {
                if let Some(tx) = &self.action_tx {
                    let _ = tx.clone().try_send(Action::AppModeChange(mode));
                }
                self.mode = mode;
            }

            // -- clear input
            if let Action::Clear = action {
                self.input.reset();
                self.filter_str = String::from("");
            }
        }

        // -- packet recieved
        if !self.dump_paused.load(Ordering::Relaxed) {
            if let Action::PacketDump(time, packet, packet_type) = action {
                match packet_type {
                    PacketTypeEnum::Tcp => self.tcp_packets.push((time, packet.clone())),
                    PacketTypeEnum::Arp => self.arp_packets.push((time, packet.clone())),
                    PacketTypeEnum::Udp => self.udp_packets.push((time, packet.clone())),
                    PacketTypeEnum::Icmp => self.icmp_packets.push((time, packet.clone())),
                    PacketTypeEnum::Icmp6 => self.icmp6_packets.push((time, packet.clone())),
                    _ => {}
                }
                self.all_packets.push((time, packet.clone()));
            }
        }

        Ok(None)
    }

    fn tab_changed(&mut self, tab: TabsEnum) -> Result<()> {
        self.active_tab = tab;
        Ok(())
    }

    fn shutdown(&mut self) -> Result<()> {
        log::info!("Shutting down packet capture component");

        // Signal thread to stop
        self.dump_stop.store(true, Ordering::SeqCst);

        // Wait for thread to finish with timeout
        if let Some(handle) = self.loop_thread.take() {
            let start = std::time::Instant::now();
            let timeout = Duration::from_secs(2);

            while !handle.is_finished() && start.elapsed() < timeout {
                thread::sleep(Duration::from_millis(50));
            }

            if handle.is_finished() {
                match handle.join() {
                    Ok(_) => log::info!("Packet capture thread stopped successfully during shutdown"),
                    Err(_) => log::error!("Packet capture thread panicked during shutdown"),
                }
            } else {
                log::warn!("Packet capture thread did not stop within timeout during shutdown");
            }
        }

        Ok(())
    }

    fn draw(&mut self, f: &mut Frame<'_>, area: Rect) -> Result<()> {
        if self.active_tab == TabsEnum::Packets {
            let layout = get_vertical_layout(area);
            let mut table_rect = layout.bottom;
            table_rect.y += 1;
            table_rect.height -= 1;

            // -- TABLE
            let mut dump_paused = false;
            if self.dump_paused.load(Ordering::Relaxed) {
                dump_paused = true;
            }
            let rows = self.get_table_rows_by_packet_type(self.packet_type);
            let table = Self::make_table(rows, self.packet_type, dump_paused);
            f.render_stateful_widget(table, table_rect, &mut self.table_state.clone());

            // -- INPUT
            let input_size: u16 = INPUT_SIZE as u16;
            let input_rect = Rect::new(
                table_rect.width - (input_size + 1),
                table_rect.y + 1,
                input_size,
                3,
            );
            // -- INPUT_SIZE - 3 is offset for border + 1char for cursor
            let scroll = self.input.visual_scroll(INPUT_SIZE - 3);
            let block = self.make_input(scroll);
            f.render_widget(block, input_rect);
            // -- cursor
            match self.mode {
                Mode::Input => {
                    f.set_cursor_position(Position {
                        x: input_rect.x
                            + ((self.input.visual_cursor()).max(scroll) - scroll) as u16
                            + 1,
                        y: input_rect.y + 1,
                    });
                }
                Mode::Normal => {}
            }

            // -- SCROLLBAR
            let scrollbar = Self::make_scrollbar();
            let mut scroll_rect = table_rect;
            scroll_rect.y += 1;
            scroll_rect.height -= 1;
            f.render_stateful_widget(
                scrollbar,
                scroll_rect.inner(Margin {
                    vertical: 1,
                    horizontal: 1,
                }),
                &mut self.scrollbar_state,
            );
        }
        Ok(())
    }
}
