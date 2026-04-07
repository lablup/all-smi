// Copyright 2025 Lablup Inc. and Jeongkyu Shin
//
// Licensed under the Apache License, Version 2.0 (the "License");
// you may not use this file except in compliance with the License.
// You may obtain a copy of the License at
//
//     http://www.apache.org/licenses/LICENSE-2.0
//
// Unless required by applicable law or agreed to in writing, software
// distributed under the License is distributed on an "AS IS" BASIS,
// WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
// See the License for the specific language governing permissions and
// limitations under the License.

use chrono::{DateTime, Local};
use std::time::Duration;

#[derive(Debug)]
pub enum NotificationError {
    InvalidDuration,
    MessageTooLong(usize),
    SystemTimeError,
}

impl std::fmt::Display for NotificationError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NotificationError::InvalidDuration => write!(f, "Invalid notification duration"),
            NotificationError::MessageTooLong(len) => {
                write!(f, "Message too long: {len} characters")
            }
            NotificationError::SystemTimeError => write!(f, "System time error"),
        }
    }
}

impl std::error::Error for NotificationError {}

#[derive(Clone, Debug)]
pub struct Notification {
    pub message: String,
    pub created_at: DateTime<Local>,
    pub duration_seconds: u64,
    #[allow(dead_code)] // Future notification system architecture
    pub notification_type: NotificationType,
}

#[derive(Clone, Debug)]
pub enum NotificationType {
    #[allow(dead_code)]
    Info,
    Warning,
    #[allow(dead_code)]
    Error,
    #[allow(dead_code)]
    Status,
}

impl Notification {
    pub fn new(
        message: String,
        notification_type: NotificationType,
    ) -> Result<Self, NotificationError> {
        Self::with_duration(message, notification_type, 4)
    }

    pub fn with_duration(
        message: String,
        notification_type: NotificationType,
        duration_seconds: u64,
    ) -> Result<Self, NotificationError> {
        const MAX_MESSAGE_LENGTH: usize = 200;

        if message.len() > MAX_MESSAGE_LENGTH {
            return Err(NotificationError::MessageTooLong(message.len()));
        }

        if duration_seconds == 0 {
            return Err(NotificationError::InvalidDuration);
        }

        Ok(Self {
            message,
            created_at: Local::now(),
            duration_seconds,
            notification_type,
        })
    }

    pub fn is_expired(&self) -> bool {
        match self.get_elapsed_time() {
            Ok(elapsed) => elapsed >= Duration::from_secs(self.duration_seconds),
            Err(_) => true, // If we can't get time, consider it expired for safety
        }
    }

    #[allow(dead_code)]
    pub fn remaining_time(&self) -> Result<Duration, NotificationError> {
        let elapsed = self.get_elapsed_time()?;
        Ok(Duration::from_secs(self.duration_seconds).saturating_sub(elapsed))
    }

    fn get_elapsed_time(&self) -> Result<Duration, NotificationError> {
        Local::now()
            .signed_duration_since(self.created_at)
            .to_std()
            .map_err(|_| NotificationError::SystemTimeError)
    }
}

#[derive(Clone, Debug, Default)]
pub struct NotificationManager {
    pub current_notification: Option<Notification>,
}

impl NotificationManager {
    pub fn new() -> Self {
        Self {
            current_notification: None,
        }
    }

    pub fn show(
        &mut self,
        message: String,
        notification_type: NotificationType,
    ) -> Result<(), NotificationError> {
        let notification = Notification::new(message, notification_type)?;
        self.current_notification = Some(notification);
        Ok(())
    }

    #[allow(dead_code)]
    pub fn show_with_duration(
        &mut self,
        message: String,
        notification_type: NotificationType,
        duration_seconds: u64,
    ) -> Result<(), NotificationError> {
        let notification =
            Notification::with_duration(message, notification_type, duration_seconds)?;
        self.current_notification = Some(notification);
        Ok(())
    }

    #[allow(dead_code)]
    pub fn clear(&mut self) {
        self.current_notification = None;
    }

    pub fn update(&mut self) {
        if let Some(notification) = &self.current_notification
            && notification.is_expired()
        {
            self.current_notification = None;
        }
    }

    pub fn get_current_message(&self) -> Option<&str> {
        self.current_notification
            .as_ref()
            .map(|n| n.message.as_str())
    }

    #[allow(dead_code)] // Future notification system architecture
    pub fn get_current_notification(&self) -> Option<&Notification> {
        self.current_notification.as_ref()
    }

    #[allow(dead_code)]
    pub fn has_notification(&self) -> bool {
        self.current_notification.is_some()
    }
}

// Helper functions for common notification types
impl NotificationManager {
    #[allow(dead_code)]
    pub fn info(&mut self, message: String) -> Result<(), NotificationError> {
        self.show(message, NotificationType::Info)
    }

    pub fn warning(&mut self, message: String) -> Result<(), NotificationError> {
        self.show(message, NotificationType::Warning)
    }

    #[allow(dead_code)]
    pub fn error(&mut self, message: String) -> Result<(), NotificationError> {
        self.show(message, NotificationType::Error)
    }

    #[allow(dead_code)]
    pub fn status(&mut self, message: String) -> Result<(), NotificationError> {
        self.show(message, NotificationType::Status)
    }

    #[allow(dead_code)]
    pub fn persistent_status(&mut self, message: String) -> Result<(), NotificationError> {
        // Use a very long duration instead of u64::MAX to avoid overflow issues
        const PERSISTENT_DURATION: u64 = 365 * 24 * 60 * 60; // 1 year in seconds
        self.show_with_duration(message, NotificationType::Status, PERSISTENT_DURATION)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_all_notification_types() {
        let mut manager = NotificationManager::new();

        // Test all notification types to ensure they work
        assert!(manager.info("Test info".to_string()).is_ok());
        assert!(manager.warning("Test warning".to_string()).is_ok());
        assert!(manager.error("Test error".to_string()).is_ok());
        assert!(manager.status("Test status".to_string()).is_ok());
        assert!(
            manager
                .persistent_status("Test persistent".to_string())
                .is_ok()
        );
    }

    #[test]
    fn test_notification_with_duration() {
        let mut manager = NotificationManager::new();
        assert!(
            manager
                .show_with_duration("Test".to_string(), NotificationType::Info, 10)
                .is_ok()
        );
    }

    #[test]
    fn test_notification_remaining_time() {
        let notification = Notification::new("Test".to_string(), NotificationType::Info).unwrap();
        assert!(notification.remaining_time().is_ok());
    }

    #[test]
    fn test_clear_notification() {
        let mut manager = NotificationManager::new();
        manager.info("Test".to_string()).unwrap();
        assert!(manager.has_notification());
        manager.clear();
        assert!(!manager.has_notification());
    }
}
