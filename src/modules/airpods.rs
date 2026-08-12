use std::{
    sync::{Arc, Mutex},
    thread,
    time::Duration,
};

use async_channel::{Receiver, Sender};
use librepods::{
    AACPManager, Address, AirPodsNoiseControlMode, BatteryComponent, BatteryStatus,
    ControlCommandIdentifiers, bluetooth::discovery::find_connected_airpods,
};
use log::{debug, warn};

const POLL_INTERVAL: Duration = Duration::from_secs(1);
const RETRY_INTERVAL: Duration = Duration::from_secs(10);

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AirPodsListeningMode {
    NoiseCancellation,
    Transparency,
    Adaptive,
}

impl AirPodsListeningMode {
    fn protocol_mode(self) -> AirPodsNoiseControlMode {
        match self {
            Self::NoiseCancellation => AirPodsNoiseControlMode::NoiseCancellation,
            Self::Transparency => AirPodsNoiseControlMode::Transparency,
            Self::Adaptive => AirPodsNoiseControlMode::Adaptive,
        }
    }

    fn from_byte(value: u8) -> Option<Self> {
        match AirPodsNoiseControlMode::from_byte(&value) {
            AirPodsNoiseControlMode::NoiseCancellation => Some(Self::NoiseCancellation),
            AirPodsNoiseControlMode::Transparency => Some(Self::Transparency),
            AirPodsNoiseControlMode::Adaptive => Some(Self::Adaptive),
            AirPodsNoiseControlMode::Off => None,
        }
    }
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct AirPodsStatus {
    pub connected: bool,
    pub ready: bool,
    pub address: Option<String>,
    pub left_percent: Option<u8>,
    pub right_percent: Option<u8>,
    pub average_percent: Option<u8>,
    pub listening_mode: Option<AirPodsListeningMode>,
    pub message: Option<String>,
}

#[derive(Clone, Copy, Debug)]
pub enum AirPodsCommand {
    SetListeningMode(AirPodsListeningMode),
}

#[derive(Clone)]
pub struct AirPodsControls {
    pub status: Arc<Mutex<AirPodsStatus>>,
    pub commands: Sender<AirPodsCommand>,
}

pub fn start_worker() -> AirPodsControls {
    let status = Arc::new(Mutex::new(AirPodsStatus::default()));
    let (command_sender, command_receiver) = async_channel::unbounded();
    let thread_status = status.clone();
    if let Err(error) = thread::Builder::new()
        .name("bah-airpods".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_multi_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    warn!("could not start AirPods runtime: {error}");
                    return;
                }
            };
            runtime.block_on(run(thread_status, command_receiver));
        })
    {
        warn!("failed to start AirPods worker: {error}");
    }
    AirPodsControls {
        status,
        commands: command_sender,
    }
}

async fn run(status: Arc<Mutex<AirPodsStatus>>, commands: Receiver<AirPodsCommand>) {
    let session = match bluer::Session::new().await {
        Ok(session) => session,
        Err(error) => {
            set_error(&status, format!("Bluetooth session unavailable: {error}"));
            return;
        }
    };
    let adapter = match session.default_adapter().await {
        Ok(adapter) => adapter,
        Err(error) => {
            set_error(&status, format!("Bluetooth adapter unavailable: {error}"));
            return;
        }
    };

    let mut manager: Option<AACPManager> = None;
    let mut active_address: Option<String> = None;
    let mut last_attempt = tokio::time::Instant::now() - RETRY_INTERVAL;

    loop {
        let discovered = find_connected_airpods(&adapter)
            .await
            .ok()
            .map(|device| device.address().to_string());

        if discovered != active_address {
            manager = None;
            active_address = discovered.clone();
            let next = AirPodsStatus {
                connected: discovered.is_some(),
                address: discovered,
                ..AirPodsStatus::default()
            };
            replace_status(&status, next);
        }

        if let Some(address) = active_address.clone()
            && manager.is_none()
            && last_attempt.elapsed() >= RETRY_INTERVAL
        {
            last_attempt = tokio::time::Instant::now();
            match connect_manager(&address).await {
                Ok(next_manager) => {
                    manager = Some(next_manager);
                    with_status(&status, |state| {
                        state.ready = true;
                        state.message = None;
                    });
                }
                Err(message) => set_error(&status, message),
            }
        }

        if let Some(manager) = &manager {
            apply_snapshot(&status, manager.snapshot().await);
        }

        while let Ok(command) = commands.try_recv() {
            let Some(manager) = &manager else {
                set_error(&status, "AirPodsを準備中です".to_string());
                continue;
            };
            match command {
                AirPodsCommand::SetListeningMode(mode) => {
                    let previous = status_snapshot(&status).listening_mode;
                    with_status(&status, |state| {
                        state.listening_mode = Some(mode);
                        state.message = None;
                    });
                    if let Err(error) = manager
                        .send_control_command(
                            ControlCommandIdentifiers::ListeningMode,
                            &[mode.protocol_mode().to_byte()],
                        )
                        .await
                    {
                        with_status(&status, |state| {
                            state.listening_mode = previous;
                            state.message = Some(format!("モードを変更できませんでした: {error}"));
                        });
                    }
                }
            }
        }

        tokio::time::sleep(POLL_INTERVAL).await;
    }
}

async fn connect_manager(address: &str) -> Result<AACPManager, String> {
    let address: Address = address
        .parse()
        .map_err(|error| format!("invalid address: {error}"))?;
    let mut manager = AACPManager::new();
    manager
        .connect(address)
        .await
        .map_err(|error| error.to_string())?;
    manager
        .send_handshake()
        .await
        .map_err(|error| error.to_string())?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    manager
        .send_set_feature_flags_packet()
        .await
        .map_err(|error| error.to_string())?;
    tokio::time::sleep(Duration::from_millis(300)).await;
    manager
        .send_notification_request()
        .await
        .map_err(|error| error.to_string())?;
    Ok(manager)
}

fn apply_snapshot(status: &Arc<Mutex<AirPodsStatus>>, snapshot: librepods::AACPStateSnapshot) {
    let mut left = None;
    let mut right = None;
    for battery in snapshot.battery_info {
        if battery.status == BatteryStatus::Disconnected {
            continue;
        }
        match battery.component {
            BatteryComponent::Left => left = Some(battery.level.min(100)),
            BatteryComponent::Right => right = Some(battery.level.min(100)),
            _ => {}
        }
    }
    let average = battery_average(left, right);
    let mode = snapshot
        .control_command_statuses
        .iter()
        .find(|command| command.identifier == ControlCommandIdentifiers::ListeningMode)
        .and_then(|command| command.value.first().copied())
        .and_then(AirPodsListeningMode::from_byte);
    with_status(status, |state| {
        state.left_percent = left;
        state.right_percent = right;
        state.average_percent = average;
        if mode.is_some() {
            state.listening_mode = mode;
        }
    });
}

fn status_snapshot(status: &Arc<Mutex<AirPodsStatus>>) -> AirPodsStatus {
    status
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .clone()
}

fn replace_status(status: &Arc<Mutex<AirPodsStatus>>, next: AirPodsStatus) {
    *status
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner()) = next;
}

fn with_status(status: &Arc<Mutex<AirPodsStatus>>, update: impl FnOnce(&mut AirPodsStatus)) {
    update(
        &mut status
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()),
    );
}

fn set_error(status: &Arc<Mutex<AirPodsStatus>>, message: String) {
    debug!("AirPods: {message}");
    with_status(status, |state| {
        state.ready = false;
        state.message = Some(message);
    });
}

fn battery_average(left: Option<u8>, right: Option<u8>) -> Option<u8> {
    match (left, right) {
        (Some(left), Some(right)) => Some(((u16::from(left) + u16::from(right)) / 2) as u8),
        (Some(value), None) | (None, Some(value)) => Some(value),
        (None, None) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::{AirPodsListeningMode, battery_average};

    #[test]
    fn averages_two_earbuds_and_retains_a_single_available_value() {
        assert_eq!(battery_average(Some(86), Some(73)), Some(79));
        assert_eq!(battery_average(Some(86), None), Some(86));
        assert_eq!(battery_average(None, Some(73)), Some(73));
        assert_eq!(battery_average(None, None), None);
    }

    #[test]
    fn listening_mode_ignores_off_and_unknown_protocol_values() {
        assert_eq!(
            AirPodsListeningMode::from_byte(0x02),
            Some(AirPodsListeningMode::NoiseCancellation)
        );
        assert_eq!(
            AirPodsListeningMode::from_byte(0x03),
            Some(AirPodsListeningMode::Transparency)
        );
        assert_eq!(
            AirPodsListeningMode::from_byte(0x04),
            Some(AirPodsListeningMode::Adaptive)
        );
        assert_eq!(AirPodsListeningMode::from_byte(0x01), None);
        assert_eq!(AirPodsListeningMode::from_byte(0xff), None);
    }
}
