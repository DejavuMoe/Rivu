//! 测试公共辅助：用字段名写输入，避免每个用例都手工拼 `Utm`。

#![allow(dead_code)]

use rivu_domain::Utm;
use rivu_source::{Channel, SourceInfo, SourceInput, classify};

#[derive(Default, Clone, Copy)]
pub struct In<'a> {
    pub host: Option<&'a str>,
    pub src: Option<&'a str>,
    pub med: Option<&'a str>,
    pub camp: Option<&'a str>,
    pub click: Option<&'a str>,
}

impl In<'_> {
    pub fn run(&self) -> SourceInfo {
        let utm = Utm {
            source: self.src.map(str::to_owned),
            medium: self.med.map(str::to_owned),
            campaign: self.camp.map(str::to_owned),
            content: None,
            term: None,
        };
        classify(&SourceInput {
            referrer_host: self.host,
            utm: &utm,
            click_id_param: self.click,
        })
    }

    pub fn channel(&self) -> Channel {
        self.run().channel
    }

    pub fn source(&self) -> String {
        self.run().referrer_source
    }
}

pub fn host(h: &str) -> In<'_> {
    In {
        host: Some(h),
        ..In::default()
    }
}

pub fn med(m: &str) -> In<'_> {
    In {
        med: Some(m),
        ..In::default()
    }
}

pub fn camp(c: &str) -> In<'_> {
    In {
        camp: Some(c),
        ..In::default()
    }
}

pub fn src(s: &str) -> In<'_> {
    In {
        src: Some(s),
        ..In::default()
    }
}
