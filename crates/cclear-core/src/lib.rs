//! CClear 核心库：扫描、规则、计划、执行与路径护栏。

extern crate self as windows_core;
pub(crate) use windows::core::{
    AsImpl, ComObject, ComObjectInner, ComObjectInterface, DYNAMIC_CAST_IID, GUID, HRESULT,
    IInspectable, IInspectable_Vtbl, IUnknown, IUnknownImpl, Interface, InterfaceRef,
    StaticComObject, imp,
};

pub mod elevation;
pub mod error;
pub mod exec;
pub mod guard;
pub mod journal;
pub mod model;
pub mod orchestrator;
pub mod rules;
pub mod scan;
pub mod settings;
pub mod special;
pub mod win;
