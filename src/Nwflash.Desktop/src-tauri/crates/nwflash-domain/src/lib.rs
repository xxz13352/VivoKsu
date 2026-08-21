mod app_page;
mod device;
mod download;
mod error;
mod firmware;
mod log;
mod operation;
mod partition;
mod quick_flash;
mod safe_flash;

pub const APP_DISPLAY_NAME: &str = "奶蛙Flash";
pub const APP_TECH_NAME: &str = "NWflash";
pub const DEFAULT_API_VERSION: &str = "0.1.0";

pub use app_page::*;
pub use device::*;
pub use download::*;
pub use error::*;
pub use firmware::*;
pub use log::*;
pub use operation::*;
pub use partition::*;
pub use quick_flash::*;
pub use safe_flash::*;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProductIdentity {
    pub display_name: &'static str,
    pub tech_name: &'static str,
    pub domain: &'static str,
}

impl Default for ProductIdentity {
    fn default() -> Self {
        Self {
            display_name: APP_DISPLAY_NAME,
            tech_name: APP_TECH_NAME,
            domain: "nwflash.cc.cd",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Page {
    pub code: &'static str,
    pub title: &'static str,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NavigationCategory {
    pub title: &'static str,
    pub pages: &'static [Page],
}

pub const IDENTIFICATION_PAGES: &[Page] = &[
    Page {
        code: "overview",
        title: "概览",
    },
    Page {
        code: "safe-flash",
        title: "安全刷写",
    },
];
