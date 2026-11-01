//! The JNI surface of the Android app.

use std::fs::File;
use std::os::fd::FromRawFd;
use std::path::PathBuf;

use jni::objects::{JClass, JIntArray, JObject, JObjectArray, JString};
use jni::sys::{jint, jstring};
use jni::JNIEnv;

use crate::pipeline::{self, PipelineOptions};

fn json_result(value: serde_json::Value) -> String {
    serde_json::to_string(&value).unwrap_or_else(|error| {
        format!("{{\"ok\":false,\"error\":\"cannot encode result: {error}\"}}")
    })
}

#[unsafe(no_mangle)]
pub extern "system" fn Java_com_sigsegv0x0b_gyrodoctor_Native_runMerge<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    inputs: JObjectArray<'local>,
    output: JString<'local>,
    options_json: JString<'local>,
    callback: JObject<'local>,
) -> jstring {
    let result = (|| -> anyhow::Result<serde_json::Value> {
        let count = env.get_array_length(&inputs)?;
        if count < 2 {
            anyhow::bail!("select at least two video chunks");
        }
        let mut paths = Vec::with_capacity(count as usize);
        for index in 0..count {
            let string = env.get_object_array_element(&inputs, index)?;
            let string = JString::from(string);
            let value: String = env.get_string(&string)?.into();
            paths.push(PathBuf::from(value));
        }
        let output: String = env.get_string(&output)?.into();
        let options_text: String = env.get_string(&options_json)?.into();
        let options: PipelineOptions = if options_text.trim().is_empty() {
            PipelineOptions::default()
        } else {
            serde_json::from_str(&options_text)?
        };
        let vm = env.get_java_vm()?;
        let mut progress = |stage: pipeline::Stage, percent: f64, message: String| {
            let Ok(mut callback_env) = vm.attach_current_thread() else {
                return;
            };
            let Ok(stage_string) = callback_env.new_string(stage.name()) else {
                return;
            };
            let Ok(message_string) = callback_env.new_string(message) else {
                return;
            };
            let _ = callback_env.call_method(
                &callback,
                "onProgress",
                "(Ljava/lang/String;Ljava/lang/String;D)V",
                &[
                    jni::objects::JValue::Object(&stage_string),
                    jni::objects::JValue::Object(&message_string),
                    jni::objects::JValue::Double(percent),
                ],
            );
        };
        let report = pipeline::merge_and_repair(
            &paths,
            PathBuf::from(&output).as_path(),
            &options,
            &mut progress,
        )?;
        Ok(serde_json::json!({"ok": true, "report": report}))
    })();

    let text = match result {
        Ok(value) => json_result(value),
        Err(error) => json_result(serde_json::json!({
            "ok": false,
            "error": format!("{error:#}"),
        })),
    };
    match env.new_string(text) {
        Ok(string) => string.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}


#[unsafe(no_mangle)]
pub extern "system" fn Java_com_sigsegv0x0b_gyrodoctor_Native_runMergeFds<'local>(
    mut env: JNIEnv<'local>,
    _class: JClass<'local>,
    input_fds: JIntArray<'local>,
    output_fd: jint,
    options_json: JString<'local>,
    callback: JObject<'local>,
) -> jstring {
    let result = (|| -> anyhow::Result<serde_json::Value> {
        let count = env.get_array_length(&input_fds)?;
        if count < 2 {
            anyhow::bail!("select at least two video chunks");
        }
        let mut raw_fds = vec![0i32; count as usize];
        env.get_int_array_region(&input_fds, 0, &mut raw_fds)?;
        let mut files = Vec::with_capacity(raw_fds.len());
        for raw_fd in raw_fds {
            let duplicate = unsafe { libc::dup(raw_fd) };
            if duplicate < 0 {
                anyhow::bail!("cannot duplicate input file descriptor {raw_fd}");
            }
            let file = unsafe { File::from_raw_fd(duplicate) };
            let size = file.metadata()?.len();
            files.push((file, usize::try_from(size)?));
        }
        let duplicate = unsafe { libc::dup(output_fd) };
        if duplicate < 0 {
            anyhow::bail!("cannot duplicate output file descriptor {output_fd}");
        }
        let mut output = unsafe { File::from_raw_fd(duplicate) };
        let options_text: String = env.get_string(&options_json)?.into();
        let options: PipelineOptions = if options_text.trim().is_empty() {
            PipelineOptions::default()
        } else {
            serde_json::from_str(&options_text)?
        };
        let vm = env.get_java_vm()?;
        let mut progress = |stage: pipeline::Stage, percent: f64, message: String| {
            let Ok(mut callback_env) = vm.attach_current_thread() else {
                return;
            };
            let Ok(stage_string) = callback_env.new_string(stage.name()) else {
                return;
            };
            let Ok(message_string) = callback_env.new_string(message) else {
                return;
            };
            let _ = callback_env.call_method(
                &callback,
                "onProgress",
                "(Ljava/lang/String;Ljava/lang/String;D)V",
                &[
                    jni::objects::JValue::Object(&stage_string),
                    jni::objects::JValue::Object(&message_string),
                    jni::objects::JValue::Double(percent),
                ],
            );
        };
        let report = pipeline::merge_and_repair_streams(
            &mut files,
            &mut output,
            "joined-repaired.mp4",
            &options,
            &mut progress,
        )?;
        Ok(serde_json::json!({"ok": true, "report": report}))
    })();

    let text = match result {
        Ok(value) => json_result(value),
        Err(error) => json_result(serde_json::json!({
            "ok": false,
            "error": format!("{error:#}"),
        })),
    };
    match env.new_string(text) {
        Ok(string) => string.into_raw(),
        Err(_) => std::ptr::null_mut(),
    }
}
