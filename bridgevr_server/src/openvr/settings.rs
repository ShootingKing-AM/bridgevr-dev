use bridgevr_common::data::*;
use log::*;
use openvr_driver_sys as vr;
use std::{ffi::*, time::*};

pub(super) const TRACE_CONTEXT: &str = "OpenVR";

const DEFAULT_EYE_RESOLUTION: (u32, u32) = (640, 720);

const DEFAULT_FOV: [Fov; 2] = [Fov {
    left: 45_f32,
    top: 45_f32,
    right: 45_f32,
    bottom: 45_f32,
}; 2];

const DEFAULT_BLOCK_STANDBY: bool = false;

// todo: use ::from_secs_f32 if it will be a const fn
const DEFAULT_FRAME_INTERVAL: Duration = Duration::from_nanos((1e9 / 60_f32) as u64);

pub struct OpenvrSettings {
    pub target_eye_resolution: (u32, u32),
    pub fov: [Fov; 2],
    pub block_standby: bool,
    pub frame_interval: Duration,
    pub hmd_custom_properties: Vec<OpenvrProp>,
    pub controllers_custom_properties: [Vec<OpenvrProp>; 2],
    pub input_mapping: [Vec<(String, InputType, Vec<String>)>; 2],
}

pub fn create_openvr_settings(
    settings: Option<&Settings>,
    session_desc: &SessionDesc,
) -> OpenvrSettings {
    let block_standby;
    let hmd_custom_properties;
    let controllers_custom_properties;
    let input_mapping;
    if let Some(settings) = settings {
        block_standby = settings.openvr.block_standby;
        hmd_custom_properties = settings.openvr.hmd_custom_properties.clone();
        controllers_custom_properties = settings.openvr.controllers_custom_properties.clone();
        input_mapping = settings.openvr.input_mapping.clone();
    } else {
        block_standby = DEFAULT_BLOCK_STANDBY;
        hmd_custom_properties = vec![];
        controllers_custom_properties = [vec![], vec![]];
        input_mapping = [vec![], vec![]];
    };

    let fov;
    let frame_interval;
    if let Some(client_handshake_packet) = &session_desc.last_client_handshake_packet {
        fov = client_handshake_packet.fov;
        frame_interval = Duration::from_secs_f32(1_f32 / client_handshake_packet.fps as f32);
    } else {
        fov = DEFAULT_FOV;
        frame_interval = DEFAULT_FRAME_INTERVAL;
    };

    let target_eye_resolution = if let Some(Settings {
        openvr:
            OpenvrDesc {
                preferred_render_eye_resolution: Some(eye_res),
                ..
            },
        ..
    }) = settings
    {
        *eye_res
    } else if let Some(client_handshake_packet) = &session_desc.last_client_handshake_packet {
        client_handshake_packet.native_eye_resolution
    } else {
        DEFAULT_EYE_RESOLUTION
    };

    OpenvrSettings {
        target_eye_resolution,
        fov,
        block_standby,
        frame_interval,
        hmd_custom_properties,
        controllers_custom_properties,
        input_mapping,
    }
}

pub fn set_custom_props(container: vr::PropertyContainerHandle_t, props: &[OpenvrProp]) {
    for prop in props {
        let res = unsafe {
            match &prop.value {
                OpenvrPropValue::Bool(value) => {
                    vr::vrSetBoolProperty(container, prop.code as _, *value)
                }
                OpenvrPropValue::Int32(value) => {
                    vr::vrSetInt32Property(container, prop.code as _, *value)
                }
                OpenvrPropValue::Uint64(value) => {
                    vr::vrSetUint64Property(container, prop.code as _, *value)
                }
                OpenvrPropValue::Float(value) => {
                    vr::vrSetFloatProperty(container, prop.code as _, *value)
                }
                OpenvrPropValue::String(value) => {
                    let c_string = CString::new(value.clone()).unwrap();
                    vr::vrSetStringProperty(container, prop.code as _, c_string.as_ptr())
                }
                OpenvrPropValue::Vector3(value) => vr::vrSetVec3Property(
                    container,
                    prop.code as _,
                    &vr::HmdVector3_t { v: *value },
                ),
                OpenvrPropValue::Matrix34(_) => todo!(),
            }
        };

        if res > 0 {
            warn!("Failed to set openvr property {:?} with code={}", prop, res);
        }
    }
}
