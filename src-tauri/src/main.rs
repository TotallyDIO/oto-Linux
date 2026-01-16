// Prevents additional console window on Windows in release
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// Module declarations
mod db;
mod models;
mod paths;
mod prompts;

// Re-exports for internal use
use db::{clear_chat_history_internal, get_chat_history_internal, store_chat_message};
use models::{ChatMessage, ChatResponse, DeepResearchResponse, TextureVersion};
use paths::*;
use prompts::*;

use rdev::{listen, Event, EventType};
use serde::Serialize;
use serde_json::{json, Value};
use std::io::{Read, Write as IoWrite};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Mutex;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder};
use tauri::{command, AppHandle, Emitter, Manager};
use tauri_plugin_global_shortcut::{Code, GlobalShortcutExt, Modifiers, Shortcut, ShortcutState};
// rusqlite is now used in db.rs module
use serde::Deserialize;

// macOS-specific imports
#[cfg(target_os = "macos")]
use objc2::rc::Retained;
#[cfg(target_os = "macos")]
use objc2_app_kit::{NSWindow, NSWindowCollectionBehavior};

// Windows-specific imports
#[cfg(target_os = "windows")]
use windows::Win32::Foundation::HWND;
#[cfg(target_os = "windows")]
use windows::Win32::UI::WindowsAndMessaging::{
    SetWindowPos, HWND_TOPMOST, SWP_NOACTIVATE, SWP_NOMOVE, SWP_NOSIZE,
};

// Path helper functions are in paths.rs module

// Default prompt constants are in prompts.rs module

// Database functions are in db.rs module
// Model structs are in models.rs module

// ============ Download Helpers ============

async fn download_and_extract_zip(url: &str, dest_dir: &PathBuf) -> Result<(), String> {
    // Download to temp file
    let response = reqwest::get(url)
        .await
        .map_err(|e| format!("Download failed: {}", e))?;

    if !response.status().is_success() {
        return Err(format!(
            "Download failed with status: {}",
            response.status()
        ));
    }

    let bytes = response
        .bytes()
        .await
        .map_err(|e| format!("Failed to read response: {}", e))?;

    // Create destination directory
    std::fs::create_dir_all(dest_dir).map_err(|e| format!("Failed to create directory: {}", e))?;

    // Extract zip
    let cursor = std::io::Cursor::new(bytes.to_vec());
    let mut archive =
        zip::ZipArchive::new(cursor).map_err(|e| format!("Failed to read zip: {}", e))?;

    for i in 0..archive.len() {
        let mut file = archive
            .by_index(i)
            .map_err(|e| format!("Failed to read zip entry: {}", e))?;

        let outpath = dest_dir.join(file.name());

        if file.name().ends_with('/') {
            std::fs::create_dir_all(&outpath)
                .map_err(|e| format!("Failed to create directory: {}", e))?;
        } else {
            if let Some(parent) = outpath.parent() {
                std::fs::create_dir_all(parent)
                    .map_err(|e| format!("Failed to create directory: {}", e))?;
            }
            let mut outfile = std::fs::File::create(&outpath)
                .map_err(|e| format!("Failed to create file: {}", e))?;
            let mut buffer = Vec::new();
            file.read_to_end(&mut buffer)
                .map_err(|e| format!("Failed to read zip content: {}", e))?;
            outfile
                .write_all(&buffer)
                .map_err(|e| format!("Failed to write file: {}", e))?;
        }
    }

    Ok(())
}

// ============ Tauri Commands ============

#[derive(Serialize)]
pub struct InitStatus {
    pub ready: bool,
    pub message: String,
    pub models_path: String,
}

#[command]
async fn init_app(app: AppHandle) -> Result<InitStatus, String> {
    let models_dir = get_models_dir()?;

    println!("[init_app] Starting initialization...");
    println!("[init_app] Models dir: {:?}", models_dir);

    // Emit progress events to frontend
    let emit_progress = |step: &str, message: &str| {
        println!("[init_app] {}: {}", step, message);
        let _ = app.emit("init-progress", json!({ "step": step, "message": message }));
    };

    // Check/download Hiyori model
    let hiyori_dir = models_dir.join("Hiyori");
    if !hiyori_dir.exists() {
        emit_progress("model", "Downloading Hiyori model...");
        match download_and_extract_zip(HIYORI_MODEL_URL, &models_dir).await {
            Ok(_) => {
                emit_progress("model", "Hiyori model ready!");
            }
            Err(e) => {
                println!("[init_app] ERROR downloading model: {}", e);
                return Err(format!("Failed to download model: {}", e));
            }
        }
    } else {
        println!("[init_app] Hiyori model already exists, skipping");
    }

    emit_progress("done", "All ready!");
    println!("[init_app] Initialization complete!");

    Ok(InitStatus {
        ready: true,
        message: "Ready".to_string(),
        models_path: models_dir.to_string_lossy().to_string(),
    })
}

#[command]
async fn get_paths() -> Result<String, String> {
    let models_dir = get_models_dir()?;
    Ok(models_dir.to_string_lossy().to_string())
}

#[command]
async fn read_file_as_text(path: String) -> Result<String, String> {
    tokio::fs::read_to_string(&path)
        .await
        .map_err(|e| format!("Failed to read file {}: {}", path, e))
}

#[command]
async fn read_file_as_bytes(path: String) -> Result<Vec<u8>, String> {
    tokio::fs::read(&path)
        .await
        .map_err(|e| format!("Failed to read file {}: {}", path, e))
}

#[command]
async fn is_initialized() -> Result<bool, String> {
    let models_dir = get_models_dir()?;
    Ok(models_dir.join("Hiyori").exists())
}

// ============ API Key Commands ============

#[command]
async fn save_api_key(key: String) -> Result<(), String> {
    let key_path = get_api_key_path()?;

    // Ensure parent directory exists
    if let Some(parent) = key_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create directory: {}", e))?;
    }

    std::fs::write(&key_path, &key).map_err(|e| format!("Failed to save API key: {}", e))?;

    Ok(())
}

#[command]
async fn get_api_key() -> Result<Option<String>, String> {
    let key_path = get_api_key_path()?;

    if key_path.exists() {
        let key = std::fs::read_to_string(&key_path)
            .map_err(|e| format!("Failed to read API key: {}", e))?;
        Ok(Some(key.trim().to_string()))
    } else {
        Ok(None)
    }
}

#[command]
async fn has_api_key() -> Result<bool, String> {
    let key_path = get_api_key_path()?;
    Ok(key_path.exists())
}

// ============ Prompt Commands ============

#[command]
async fn save_system_prompt(prompt: String) -> Result<(), String> {
    let prompt_path = get_system_prompt_path()?;

    if let Some(parent) = prompt_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create directory: {}", e))?;
    }

    std::fs::write(&prompt_path, &prompt)
        .map_err(|e| format!("Failed to save system prompt: {}", e))?;

    Ok(())
}

#[command]
async fn get_system_prompt() -> Result<String, String> {
    let prompt_path = get_system_prompt_path()?;

    if prompt_path.exists() {
        let prompt = std::fs::read_to_string(&prompt_path)
            .map_err(|e| format!("Failed to read system prompt: {}", e))?;
        let trimmed = prompt.trim().to_string();
        if trimmed.is_empty() {
            Ok(DEFAULT_SYSTEM_PROMPT.to_string())
        } else {
            Ok(trimmed)
        }
    } else {
        Ok(DEFAULT_SYSTEM_PROMPT.to_string())
    }
}

#[command]
async fn save_character_prompt(prompt: String) -> Result<(), String> {
    let prompt_path = get_character_prompt_path()?;

    if let Some(parent) = prompt_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create directory: {}", e))?;
    }

    std::fs::write(&prompt_path, &prompt)
        .map_err(|e| format!("Failed to save character prompt: {}", e))?;

    Ok(())
}

#[command]
async fn get_character_prompt() -> Result<String, String> {
    let prompt_path = get_character_prompt_path()?;

    if prompt_path.exists() {
        let prompt = std::fs::read_to_string(&prompt_path)
            .map_err(|e| format!("Failed to read character prompt: {}", e))?;
        let trimmed = prompt.trim().to_string();
        if trimmed.is_empty() {
            Ok(DEFAULT_CHARACTER_PROMPT.to_string())
        } else {
            Ok(trimmed)
        }
    } else {
        Ok(DEFAULT_CHARACTER_PROMPT.to_string())
    }
}

#[command]
async fn save_deep_research_prompt(prompt: String) -> Result<(), String> {
    let prompt_path = get_deep_research_prompt_path()?;

    if let Some(parent) = prompt_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create directory: {}", e))?;
    }

    std::fs::write(&prompt_path, &prompt)
        .map_err(|e| format!("Failed to save deep research prompt: {}", e))?;

    Ok(())
}

#[command]
async fn get_deep_research_prompt() -> Result<String, String> {
    let prompt_path = get_deep_research_prompt_path()?;

    if prompt_path.exists() {
        let prompt = std::fs::read_to_string(&prompt_path)
            .map_err(|e| format!("Failed to read deep research prompt: {}", e))?;
        let trimmed = prompt.trim().to_string();
        if trimmed.is_empty() {
            Ok(DEFAULT_DEEP_RESEARCH_PROMPT.to_string())
        } else {
            Ok(trimmed)
        }
    } else {
        Ok(DEFAULT_DEEP_RESEARCH_PROMPT.to_string())
    }
}

#[command]
async fn save_dialogue_prompt(prompt: String) -> Result<(), String> {
    let prompt_path = get_dialogue_prompt_path()?;

    if let Some(parent) = prompt_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create directory: {}", e))?;
    }

    std::fs::write(&prompt_path, &prompt)
        .map_err(|e| format!("Failed to save dialogue prompt: {}", e))?;

    Ok(())
}

#[command]
async fn get_dialogue_prompt() -> Result<String, String> {
    let prompt_path = get_dialogue_prompt_path()?;

    if prompt_path.exists() {
        let prompt = std::fs::read_to_string(&prompt_path)
            .map_err(|e| format!("Failed to read dialogue prompt: {}", e))?;
        let trimmed = prompt.trim().to_string();
        if trimmed.is_empty() {
            Ok(DEFAULT_DIALOGUE_PROMPT.to_string())
        } else {
            Ok(trimmed)
        }
    } else {
        Ok(DEFAULT_DIALOGUE_PROMPT.to_string())
    }
}

// ============ Hitbox Commands ============

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Point2D {
    x: f64,
    y: f64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HitboxData {
    points: Vec<Point2D>,
}

#[command]
async fn save_hitbox(points: Vec<Point2D>) -> Result<(), String> {
    let hitbox_path = get_hitbox_path()?;

    if let Some(parent) = hitbox_path.parent() {
        std::fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create directory: {}", e))?;
    }

    let data = HitboxData { points };
    let json = serde_json::to_string_pretty(&data)
        .map_err(|e| format!("Failed to serialize hitbox: {}", e))?;

    std::fs::write(&hitbox_path, json).map_err(|e| format!("Failed to save hitbox: {}", e))?;

    println!("[Hitbox] Saved {} points", data.points.len());
    Ok(())
}

#[command]
async fn load_hitbox() -> Result<Option<HitboxData>, String> {
    let hitbox_path = get_hitbox_path()?;

    if !hitbox_path.exists() {
        return Ok(None);
    }

    let json = std::fs::read_to_string(&hitbox_path)
        .map_err(|e| format!("Failed to read hitbox: {}", e))?;

    let data: HitboxData =
        serde_json::from_str(&json).map_err(|e| format!("Failed to parse hitbox: {}", e))?;

    println!("[Hitbox] Loaded {} points", data.points.len());
    Ok(Some(data))
}

#[command]
async fn clear_hitbox() -> Result<(), String> {
    let hitbox_path = get_hitbox_path()?;

    if hitbox_path.exists() {
        std::fs::remove_file(&hitbox_path).map_err(|e| format!("Failed to clear hitbox: {}", e))?;
        println!("[Hitbox] Cleared hitbox");
    }

    Ok(())
}

// ============ Chat Commands ============

#[command]
async fn send_chat_message(
    app: AppHandle,
    message: String,
    include_screenshot: bool,
    context_level: u8,
) -> Result<ChatResponse, String> {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};

    // Get API key
    let api_key = get_api_key()
        .await?
        .ok_or_else(|| "API key not configured".to_string())?;

    // Get system prompt based on level
    let system_prompt = match context_level {
        1 => {
            // Level 1: Use dialogue prompt (respond AS Miku in direct conversation)
            get_dialogue_prompt().await?
        }
        2 => {
            // Level 2: Use deep research prompt (respond as analyst)
            get_deep_research_prompt().await?
        }
        _ => {
            // Level 0: Default system prompt
            get_system_prompt().await?
        }
    };

    // Take screenshot if enabled (only for level 0)
    let screenshot_base64 = if include_screenshot && context_level == 0 {
        let screenshot_path = take_screenshot(app).await?;
        let screenshot_bytes = std::fs::read(&screenshot_path)
            .map_err(|e| format!("Failed to read screenshot: {}", e))?;
        Some(BASE64.encode(&screenshot_bytes))
    } else {
        None
    };

    // Get recent chat history for context
    let history = get_chat_history_internal(10)?;

    // Build messages array with system prompt
    let mut messages: Vec<Value> = vec![json!({
        "role": "system",
        "content": system_prompt
    })];

    // Add past messages for context, filtered by level
    for msg in &history {
        let include_msg = match context_level {
            1 => {
                // Level 1: User + miku + assistant (includes AI responses for context)
                msg.role == "user" || msg.role == "miku" || msg.role == "assistant"
            }
            2 => {
                // Level 2: Only user + deep-thought messages
                msg.role == "user" || msg.role == "deep-thought"
            }
            _ => {
                // Level 0: All except deep-thought
                msg.role != "deep-thought"
            }
        };

        if !include_msg {
            continue;
        }

        // Convert custom roles to "assistant" for API compatibility
        // Add distinct labels for level 1 context so Miku knows what's what
        let (role, content) = if msg.role == "miku" {
            if context_level == 1 {
                (
                    "assistant",
                    format!("[Miku's Inner Thoughts]: {}", msg.content),
                )
            } else {
                ("assistant", format!("[Miku]: {}", msg.content))
            }
        } else if msg.role == "assistant" && context_level == 1 {
            // For Level 1, format assistant messages distinctly
            (
                "assistant",
                format!("[AI Assistant Response]: {}", msg.content),
            )
        } else if msg.role == "deep-thought" {
            ("assistant", format!("[Analysis]: {}", msg.content))
        } else {
            (msg.role.as_str(), msg.content.clone())
        };

        messages.push(json!({
            "role": role,
            "content": content
        }));
    }

    // Add current message (with or without screenshot)
    if let Some(ref base64) = screenshot_base64 {
        messages.push(json!({
            "role": "user",
            "content": [
                {
                    "type": "text",
                    "text": message.clone()
                },
                {
                    "type": "image_url",
                    "image_url": {
                        "url": format!("data:image/png;base64,{}", base64)
                    }
                }
            ]
        }));
    } else {
        messages.push(json!({
            "role": "user",
            "content": message.clone()
        }));
    }

    // Call OpenAI API for main response
    let client = reqwest::Client::new();
    let response = client
        .post("https://api.openai.com/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&json!({
            "model": "gpt-4.1-2025-04-14",
            "messages": messages,
            "max_tokens": 1000
        }))
        .send()
        .await
        .map_err(|e| format!("API request failed: {}", e))?;

    if !response.status().is_success() {
        let error_text = response.text().await.unwrap_or_default();
        return Err(format!("API error: {}", error_text));
    }

    let response_json: Value = response
        .json()
        .await
        .map_err(|e| format!("Failed to parse response: {}", e))?;

    let main_response = response_json["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("No response")
        .to_string();

    // Store messages and generate character comments based on level
    let timestamp = chrono::Utc::now().to_rfc3339();
    store_chat_message(&timestamp, "user", &message, context_level)?;

    let character_comments = match context_level {
        1 => {
            // Level 1: Save response as "miku", no separate character comments
            store_chat_message(&timestamp, "miku", &main_response, 1)?;
            None
        }
        2 => {
            // Level 2: Save response as "deep-thought", no character comments
            store_chat_message(&timestamp, "deep-thought", &main_response, 2)?;
            None
        }
        _ => {
            // Level 0: Save as "assistant", then generate Miku comment
            store_chat_message(&timestamp, "assistant", &main_response, 0)?;

            // Generate Miku commentary for level 0 only
            let char_system_prompt = get_character_prompt().await?;

            let char_messages: Vec<Value> = vec![
                json!({
                    "role": "system",
                    "content": char_system_prompt
                }),
                json!({
                    "role": "user",
                    "content": format!("Here is the AI response to comment on:\n\n{}", main_response)
                }),
            ];

            let char_response = client
                .post("https://api.openai.com/v1/chat/completions")
                .header("Authorization", format!("Bearer {}", api_key))
                .header("Content-Type", "application/json")
                .json(&json!({
                    "model": "gpt-4.1-2025-04-14",
                    "messages": char_messages,
                    "max_tokens": 500
                }))
                .send()
                .await;

            match char_response {
                Ok(resp) if resp.status().is_success() => {
                    if let Ok(char_json) = resp.json::<Value>().await {
                        let char_content = char_json["choices"][0]["message"]["content"]
                            .as_str()
                            .unwrap_or("");
                        if !char_content.is_empty() {
                            // Store Miku comment at level 0
                            store_chat_message(&timestamp, "miku", char_content, 0)?;
                            // Return as single comment at end (not randomly inserted)
                            Some(vec![char_content.trim().to_string()])
                        } else {
                            None
                        }
                    } else {
                        None
                    }
                }
                _ => None,
            }
        }
    };

    Ok(ChatResponse {
        main_response,
        character_comments,
    })
}

// Database helper functions (store_chat_message, get_chat_history_internal) are in db.rs

#[command]
async fn get_chat_history() -> Result<Vec<ChatMessage>, String> {
    get_chat_history_internal(100)
}

#[command]
async fn clear_chat_history() -> Result<(), String> {
    clear_chat_history_internal()
}

#[command]
async fn trigger_deep_research() -> Result<DeepResearchResponse, String> {
    use std::time::{SystemTime, UNIX_EPOCH};

    let cooldown_path = get_deep_research_cooldown_path()?;
    let six_hours: u64 = 6 * 60 * 60;
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| e.to_string())?
        .as_secs();

    // Check cooldown
    if cooldown_path.exists() {
        let last_time_str = std::fs::read_to_string(&cooldown_path).map_err(|e| e.to_string())?;
        if let Ok(last_time) = last_time_str.parse::<u64>() {
            if now - last_time < six_hours {
                let remaining = six_hours - (now - last_time);
                // Return cooldown status - frontend will show timer and existing deep thought
                return Ok(DeepResearchResponse {
                    on_cooldown: true,
                    remaining_seconds: remaining,
                    main_response: String::new(),
                });
            }
        }
    }

    // Not on cooldown - run deep research
    let api_key = get_api_key().await?.ok_or("API key not configured")?;
    let deep_prompt = get_deep_research_prompt().await?;
    let history = get_chat_history_internal(50)?;

    let context = history
        .iter()
        .map(|m| format!("[{}]: {}", m.role, m.content))
        .collect::<Vec<_>>()
        .join("\n\n");

    let client = reqwest::Client::new();
    let response = client
        .post("https://api.openai.com/v1/chat/completions")
        .header("Authorization", format!("Bearer {}", api_key))
        .header("Content-Type", "application/json")
        .json(&serde_json::json!({
            "model": "gpt-4o",
            "messages": [
                { "role": "system", "content": deep_prompt },
                { "role": "user", "content": format!("Analyze this conversation history:\n\n{}", context) }
            ]
        }))
        .send()
        .await
        .map_err(|e| e.to_string())?;

    if !response.status().is_success() {
        let error_text = response.text().await.unwrap_or_default();
        eprintln!("[DeepResearch] API error: {}", error_text);
        return Err(format!("API request failed: {}", error_text));
    }

    let response_json: serde_json::Value = response.json().await.map_err(|e| e.to_string())?;
    let insights = response_json["choices"][0]["message"]["content"]
        .as_str()
        .unwrap_or("No insights generated")
        .to_string();

    // Store with deep-thought marker at level 2
    let timestamp = chrono::Utc::now().to_rfc3339();
    store_chat_message(&timestamp, "deep-thought", &insights, 2)?;

    // Update cooldown timestamp
    if let Some(parent) = cooldown_path.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&cooldown_path, now.to_string()).map_err(|e| e.to_string())?;

    Ok(DeepResearchResponse {
        on_cooldown: false,
        remaining_seconds: 0,
        main_response: insights,
    })
}

#[command]
async fn clear_all_data() -> Result<(), String> {
    clear_app_data()
}

#[command]
async fn generate_texture(prompt: String) -> Result<String, String> {
    use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
    use image::GenericImageView;

    let texture_dir = get_texture_dir()?;
    let originals_dir = get_originals_dir()?;

    // Get OpenAI API key
    let api_key = get_api_key()
        .await?
        .ok_or_else(|| "No API key configured".to_string())?;

    // Process both texture files
    let texture_files = ["hiyori_texture_00.png", "hiyori_texture_01.png"];

    for texture_file in &texture_files {
        let texture_path = texture_dir.join(texture_file);
        let original_path = originals_dir.join(texture_file);

        // Ensure we have originals backed up first
        if !original_path.exists() {
            if texture_path.exists() {
                std::fs::create_dir_all(&originals_dir)
                    .map_err(|e| format!("Failed to create originals dir: {}", e))?;
                std::fs::copy(&texture_path, &original_path)
                    .map_err(|e| format!("Failed to backup {}: {}", texture_file, e))?;
            } else {
                continue;
            }
        }

        // Load the original image
        let img = image::open(&original_path)
            .map_err(|e| format!("Failed to load {}: {}", texture_file, e))?;

        let (orig_width, orig_height) = img.dimensions();
        println!(
            "[Texture] Processing {} - original dimensions: {}x{}",
            texture_file, orig_width, orig_height
        );

        // Downscale to 1024x1024 for OpenAI
        println!("[Texture] Downscaling to 1024x1024...");
        let downscaled = img.resize_exact(1024, 1024, image::imageops::FilterType::Lanczos3);

        // Encode as PNG bytes
        let mut png_bytes: Vec<u8> = Vec::new();
        downscaled
            .write_to(
                &mut std::io::Cursor::new(&mut png_bytes),
                image::ImageFormat::Png,
            )
            .map_err(|e| format!("Failed to encode image: {}", e))?;

        // Create multipart form for OpenAI API
        let form = reqwest::multipart::Form::new()
            .text("model", "gpt-image-1.5")
            .text(
                "prompt",
                format!(
                    "This is a texture atlas for a Live2D anime character. {}. \
                CRITICAL: Keep every element in its EXACT position. \
                Preserve all black outlines/lineart. \
                Only modify what the prompt asks for. \
                Maintain the same art style and quality.",
                    prompt
                ),
            )
            .text("size", "1024x1024")
            .text("background", "transparent")
            .text("output_format", "png")
            .part(
                "image[]",
                reqwest::multipart::Part::bytes(png_bytes)
                    .file_name("texture.png")
                    .mime_str("image/png")
                    .map_err(|e| format!("Failed to set mime type: {}", e))?,
            );

        // Call OpenAI API
        println!("[Texture] Sending to OpenAI...");
        let client = reqwest::Client::new();
        let response = client
            .post("https://api.openai.com/v1/images/edits")
            .header("Authorization", format!("Bearer {}", api_key))
            .multipart(form)
            .send()
            .await
            .map_err(|e| format!("OpenAI API failed for {}: {}", texture_file, e))?;

        if !response.status().is_success() {
            let error_text = response.text().await.unwrap_or_default();
            return Err(format!(
                "OpenAI API error for {}: {}",
                texture_file, error_text
            ));
        }

        let response_json: Value = response
            .json()
            .await
            .map_err(|e| format!("Failed to parse response for {}: {}", texture_file, e))?;

        println!("[Texture] Response received, extracting image...");

        // Extract base64 image from response
        let image_data = response_json["data"][0]["b64_json"]
            .as_str()
            .ok_or_else(|| format!("No image in response for {}", texture_file))?;

        // Decode the edited image
        let decoded = BASE64
            .decode(image_data)
            .map_err(|e| format!("Failed to decode {}: {}", texture_file, e))?;

        let edited_img = image::load_from_memory(&decoded)
            .map_err(|e| format!("Failed to load edited {}: {}", texture_file, e))?;

        // Upscale back to original dimensions (2048x2048)
        println!(
            "[Texture] Upscaling back to {}x{}...",
            orig_width, orig_height
        );
        let upscaled = edited_img.resize_exact(
            orig_width,
            orig_height,
            image::imageops::FilterType::Lanczos3,
        );

        // Save the upscaled image
        upscaled
            .save(&texture_path)
            .map_err(|e| format!("Failed to save {}: {}", texture_file, e))?;

        println!("[Texture] {} completed successfully", texture_file);
    }

    // Save this generation as a version
    let timestamp = chrono::Local::now().format("%Y-%m-%d_%H-%M-%S").to_string();
    let version_dir = get_versions_dir()?.join(&timestamp);
    std::fs::create_dir_all(&version_dir)
        .map_err(|e| format!("Failed to create version dir: {}", e))?;

    // Copy all processed textures to the version folder
    for texture_file in &texture_files {
        let src = texture_dir.join(texture_file);
        let dst = version_dir.join(texture_file);
        if src.exists() {
            std::fs::copy(&src, &dst)
                .map_err(|e| format!("Failed to copy {} to version: {}", texture_file, e))?;
        }
    }

    // Save metadata
    let metadata = json!({
        "timestamp": timestamp,
        "prompt": prompt,
        "created_at": chrono::Utc::now().to_rfc3339()
    });
    std::fs::write(version_dir.join("metadata.json"), metadata.to_string())
        .map_err(|e| format!("Failed to save metadata: {}", e))?;

    Ok("Texture generated successfully!".to_string())
}

#[derive(Serialize)]
pub struct TexturePaths {
    pub current_00: Option<String>,
    pub current_01: Option<String>,
    pub original_00: Option<String>,
    pub original_01: Option<String>,
    pub has_original: bool,
}

#[command]
async fn get_texture_paths() -> Result<TexturePaths, String> {
    let texture_dir = get_texture_dir()?;
    let originals_dir = get_originals_dir()?;

    let current_00_path = texture_dir.join("hiyori_texture_00.png");
    let current_01_path = texture_dir.join("hiyori_texture_01.png");
    let original_00_path = originals_dir.join("hiyori_texture_00.png");
    let original_01_path = originals_dir.join("hiyori_texture_01.png");

    Ok(TexturePaths {
        current_00: if current_00_path.exists() {
            Some(current_00_path.to_string_lossy().to_string())
        } else {
            None
        },
        current_01: if current_01_path.exists() {
            Some(current_01_path.to_string_lossy().to_string())
        } else {
            None
        },
        original_00: if original_00_path.exists() {
            Some(original_00_path.to_string_lossy().to_string())
        } else {
            None
        },
        original_01: if original_01_path.exists() {
            Some(original_01_path.to_string_lossy().to_string())
        } else {
            None
        },
        has_original: original_00_path.exists(),
    })
}

#[command]
async fn reload_character(
    app: AppHandle,
    state: tauri::State<'_, AppState>,
) -> Result<String, String> {
    println!("[Rust] reload_character called");

    // Close existing overlay window if it exists
    if let Some(overlay) = app.get_webview_window("overlay") {
        println!("[Rust] Closing existing overlay window");
        overlay
            .close()
            .map_err(|e| format!("Failed to close overlay: {}", e))?;

        // Small delay to ensure window is closed
        tokio::time::sleep(tokio::time::Duration::from_millis(100)).await;
    }

    println!("[Rust] Creating new overlay window");

    // Recreate the overlay window with fresh state
    let overlay = tauri::WebviewWindowBuilder::new(
        &app,
        "overlay",
        tauri::WebviewUrl::App("overlay.html".into()),
    )
    .title("Overlay")
    .visible(false)
    .transparent(true)
    .decorations(false)
    .shadow(false)
    .always_on_top(true)
    .skip_taskbar(true)
    .inner_size(400.0, 600.0)
    .resizable(true)
    .build()
    .map_err(|e| format!("Failed to create overlay window: {}", e))?;

    println!("[Rust] New overlay window created, configuring...");

    // Configure the overlay (make it click-through, etc.)
    configure_overlay(&overlay)?;

    // Position in bottom right of screen
    if let Ok(Some(monitor)) = overlay.current_monitor() {
        let screen_size = monitor.size();
        let screen_pos = monitor.position();
        if let Ok(window_size) = overlay.outer_size() {
            let x = screen_pos.x + (screen_size.width as i32) - (window_size.width as i32);
            let y = screen_pos.y + (screen_size.height as i32) - (window_size.height as i32);
            let _ =
                overlay.set_position(tauri::Position::Physical(tauri::PhysicalPosition { x, y }));
        }
    }

    // Show the overlay
    overlay
        .show()
        .map_err(|e| format!("Failed to show overlay: {}", e))?;

    // Update state
    *state.overlay_visible.lock().unwrap() = true;

    // Wait for page to fully load before emitting init-complete
    println!("[Rust] Waiting for overlay page to load...");
    tokio::time::sleep(tokio::time::Duration::from_millis(500)).await;

    // Emit init-complete to trigger model loading
    println!("[Rust] Emitting init-complete to load model");
    overlay
        .emit("init-complete", json!({}))
        .map_err(|e| format!("Failed to emit init-complete: {}", e))?;

    println!("[Rust] Overlay recreated successfully");
    Ok("Character reloaded!".to_string())
}

// TextureVersion struct is in models.rs

#[command]
async fn get_texture_versions() -> Result<Vec<TextureVersion>, String> {
    let versions_dir = get_versions_dir()?;
    let originals_dir = get_originals_dir()?;

    let mut versions = Vec::new();

    // Add generated versions
    if versions_dir.exists() {
        for entry in std::fs::read_dir(&versions_dir).map_err(|e| e.to_string())? {
            let entry = entry.map_err(|e| e.to_string())?;
            if entry.path().is_dir() {
                let id = entry.file_name().to_string_lossy().to_string();
                let metadata_path = entry.path().join("metadata.json");
                let (created_at, prompt) = if metadata_path.exists() {
                    let content = std::fs::read_to_string(&metadata_path).unwrap_or_default();
                    let json: Value = serde_json::from_str(&content).unwrap_or(json!({}));
                    (
                        json["created_at"].as_str().unwrap_or(&id).to_string(),
                        json["prompt"].as_str().map(|s| s.to_string()),
                    )
                } else {
                    (id.clone(), None)
                };
                versions.push(TextureVersion {
                    id,
                    created_at,
                    prompt,
                });
            }
        }
    }

    versions.sort_by(|a, b| b.id.cmp(&a.id)); // Newest first

    // Add "original" as the last option if originals exist
    if originals_dir.exists() && originals_dir.join("hiyori_texture_00.png").exists() {
        versions.push(TextureVersion {
            id: "original".to_string(),
            created_at: "Original".to_string(),
            prompt: Some("Original textures".to_string()),
        });
    }

    Ok(versions)
}

#[command]
async fn apply_texture_version(version_id: String) -> Result<String, String> {
    let texture_dir = get_texture_dir()?;
    let texture_files = ["hiyori_texture_00.png", "hiyori_texture_01.png"];

    // Handle "original" as a special case
    let source_dir = if version_id == "original" {
        get_originals_dir()?
    } else {
        get_versions_dir()?.join(&version_id)
    };

    if !source_dir.exists() {
        return Err("Version not found".to_string());
    }

    for texture_file in &texture_files {
        let src = source_dir.join(texture_file);
        let dst = texture_dir.join(texture_file);
        if src.exists() {
            std::fs::copy(&src, &dst)
                .map_err(|e| format!("Failed to apply {}: {}", texture_file, e))?;
        }
    }

    Ok(format!(
        "Applied {}",
        if version_id == "original" {
            "original textures"
        } else {
            &version_id
        }
    ))
}

#[command]
async fn delete_texture_version(version_id: String) -> Result<String, String> {
    // Prevent deleting the original
    if version_id == "original" {
        return Err("Cannot delete original textures".to_string());
    }

    let versions_dir = get_versions_dir()?;
    let version_path = versions_dir.join(&version_id);

    if !version_path.exists() {
        return Err("Version not found".to_string());
    }

    std::fs::remove_dir_all(&version_path).map_err(|e| format!("Failed to delete: {}", e))?;

    Ok("Version deleted".to_string())
}

// ============ App State ============

#[derive(Default)]
pub struct AppState {
    pub overlay_visible: Mutex<bool>,
    pub toggle_menu_item: Mutex<Option<MenuItem<tauri::Wry>>>,
}

// ============ Overlay Window Commands ============

#[cfg(target_os = "macos")]
fn configure_overlay(window: &tauri::WebviewWindow) -> Result<(), String> {
    window
        .with_webview(|webview| unsafe {
            let ns_window_ptr = webview.ns_window();
            let ns_window: Retained<NSWindow> =
                Retained::retain(ns_window_ptr as *mut NSWindow).unwrap();

            let behavior = NSWindowCollectionBehavior::CanJoinAllSpaces
                | NSWindowCollectionBehavior::FullScreenAuxiliary;
            ns_window.setCollectionBehavior(behavior);
            ns_window.setLevel(1000);
        })
        .map_err(|e| format!("Failed to configure overlay: {}", e))?;
    Ok(())
}

#[cfg(target_os = "windows")]
fn configure_overlay(window: &tauri::WebviewWindow) -> Result<(), String> {
    let hwnd = window
        .hwnd()
        .map_err(|e| format!("Failed to get HWND: {}", e))?;
    unsafe {
        SetWindowPos(
            HWND(hwnd.0 as *mut std::ffi::c_void),
            HWND_TOPMOST,
            0,
            0,
            0,
            0,
            SWP_NOMOVE | SWP_NOSIZE | SWP_NOACTIVATE,
        )
        .map_err(|e| format!("SetWindowPos failed: {}", e))?;
    }
    Ok(())
}

#[cfg(not(any(target_os = "macos", target_os = "windows")))]
fn configure_overlay(_window: &tauri::WebviewWindow) -> Result<(), String> {
    Ok(())
}

#[command]
async fn show_overlay(app: AppHandle, state: tauri::State<'_, AppState>) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("overlay") {
        configure_overlay(&window)?;

        // Position in bottom right of screen
        if let Ok(Some(monitor)) = window.current_monitor() {
            let screen_size = monitor.size();
            let screen_pos = monitor.position();
            if let Ok(window_size) = window.outer_size() {
                let x = screen_pos.x + (screen_size.width as i32) - (window_size.width as i32);
                let y = screen_pos.y + (screen_size.height as i32) - (window_size.height as i32);
                let _ = window
                    .set_position(tauri::Position::Physical(tauri::PhysicalPosition { x, y }));
            }
        }

        window.show().map_err(|e| e.to_string())?;
        window.set_focus().map_err(|e| e.to_string())?;

        // Update state
        *state.overlay_visible.lock().unwrap() = true;

        // Update tray menu text
        if let Some(menu_item) = state.toggle_menu_item.lock().unwrap().as_ref() {
            let _ = menu_item.set_text("Hide Character");
        }

        // Emit event
        let _ = app.emit("overlay-visibility-changed", json!({ "visible": true }));
    }
    Ok(())
}

#[command]
async fn hide_overlay(app: AppHandle, state: tauri::State<'_, AppState>) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("overlay") {
        window.hide().map_err(|e| e.to_string())?;

        // Update state
        *state.overlay_visible.lock().unwrap() = false;

        // Update tray menu text
        if let Some(menu_item) = state.toggle_menu_item.lock().unwrap().as_ref() {
            let _ = menu_item.set_text("Show Character");
        }

        // Emit event
        let _ = app.emit("overlay-visibility-changed", json!({ "visible": false }));
    }
    Ok(())
}

#[command]
async fn toggle_overlay(app: AppHandle, state: tauri::State<'_, AppState>) -> Result<bool, String> {
    let is_visible = *state.overlay_visible.lock().unwrap();

    if is_visible {
        hide_overlay(app, state).await?;
        Ok(false)
    } else {
        show_overlay(app, state).await?;
        Ok(true)
    }
}

// Sync version for use in tray handlers (non-async context)
fn toggle_overlay_sync(app: &AppHandle) {
    let state = app.state::<AppState>();
    let is_visible = *state.overlay_visible.lock().unwrap();

    if is_visible {
        if let Some(window) = app.get_webview_window("overlay") {
            let _ = window.hide();
            *state.overlay_visible.lock().unwrap() = false;
            let _ = app.emit("overlay-visibility-changed", json!({ "visible": false }));

            // Update tray menu text
            if let Some(menu_item) = state.toggle_menu_item.lock().unwrap().as_ref() {
                let _ = menu_item.set_text("Show Character");
            }
        }
    } else if let Some(window) = app.get_webview_window("overlay") {
        let _ = configure_overlay(&window);

        // Position in bottom right of screen
        if let Ok(Some(monitor)) = window.current_monitor() {
            let screen_size = monitor.size();
            let screen_pos = monitor.position();
            if let Ok(window_size) = window.outer_size() {
                let x = screen_pos.x + (screen_size.width as i32) - (window_size.width as i32);
                let y = screen_pos.y + (screen_size.height as i32) - (window_size.height as i32);
                let _ = window
                    .set_position(tauri::Position::Physical(tauri::PhysicalPosition { x, y }));
            }
        }

        let _ = window.show();
        let _ = window.set_focus();
        *state.overlay_visible.lock().unwrap() = true;
        let _ = app.emit("overlay-visibility-changed", json!({ "visible": true }));

        // Update tray menu text
        if let Some(menu_item) = state.toggle_menu_item.lock().unwrap().as_ref() {
            let _ = menu_item.set_text("Hide Character");
        }
    }
}

#[command]
async fn get_overlay_visible(state: tauri::State<'_, AppState>) -> Result<bool, String> {
    Ok(*state.overlay_visible.lock().unwrap())
}

#[command]
async fn hide_main_window(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        window.hide().map_err(|e| e.to_string())?;
        let _ = app.emit(
            "main-window-visibility-changed",
            json!({ "visible": false }),
        );
    }
    Ok(())
}

#[command]
async fn show_main_window(app: AppHandle) -> Result<(), String> {
    if let Some(window) = app.get_webview_window("main") {
        window.show().map_err(|e| e.to_string())?;
        window.set_focus().map_err(|e| e.to_string())?;
        let _ = app.emit("main-window-visibility-changed", json!({ "visible": true }));
    }
    Ok(())
}

#[command]
async fn toggle_main_window(app: AppHandle) -> Result<bool, String> {
    if let Some(window) = app.get_webview_window("main") {
        let is_visible = window.is_visible().map_err(|e| e.to_string())?;
        if is_visible {
            window.hide().map_err(|e| e.to_string())?;
            let _ = app.emit(
                "main-window-visibility-changed",
                json!({ "visible": false }),
            );
            Ok(false)
        } else {
            window.show().map_err(|e| e.to_string())?;
            window.set_focus().map_err(|e| e.to_string())?;
            let _ = app.emit("main-window-visibility-changed", json!({ "visible": true }));
            Ok(true)
        }
    } else {
        Ok(false)
    }
}

#[command]
async fn is_main_window_visible(app: AppHandle) -> Result<bool, String> {
    if let Some(window) = app.get_webview_window("main") {
        window.is_visible().map_err(|e| e.to_string())
    } else {
        Ok(false)
    }
}

// ============ Device Listening ============

#[derive(Debug, Clone, Serialize)]
pub struct DeviceEvent {
    kind: String,
    value: Value,
}

static IS_LISTENING: AtomicBool = AtomicBool::new(false);

#[command]
async fn start_device_listening(app: AppHandle) -> Result<(), String> {
    if IS_LISTENING.load(Ordering::SeqCst) {
        return Ok(());
    }
    IS_LISTENING.store(true, Ordering::SeqCst);

    std::thread::spawn(move || {
        let callback = move |event: Event| {
            // Mouse tracking for head movement
            if let EventType::MouseMove { x, y } = event.event_type {
                let device_event = DeviceEvent {
                    kind: "MouseMove".to_string(),
                    value: json!({ "x": x, "y": y }),
                };
                let _ = app.emit("device-changed", device_event);
            }
        };
        listen(callback).ok();
    });

    Ok(())
}

// ============ Screenshot ============

// Native macOS screen capture permission APIs
#[cfg(target_os = "macos")]
#[link(name = "CoreGraphics", kind = "framework")]
extern "C" {
    fn CGPreflightScreenCaptureAccess() -> bool;
    fn CGRequestScreenCaptureAccess() -> bool;
}

#[command]
async fn check_screen_permission() -> Result<bool, String> {
    #[cfg(target_os = "macos")]
    {
        unsafe {
            // First check if we already have permission
            if CGPreflightScreenCaptureAccess() {
                return Ok(true);
            }
            // If not, request permission (triggers system dialog)
            Ok(CGRequestScreenCaptureAccess())
        }
    }

    #[cfg(not(target_os = "macos"))]
    {
        Ok(true)
    }
}

#[command]
async fn open_screen_recording_settings() -> Result<(), String> {
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_ScreenCapture")
            .spawn()
            .map_err(|e| format!("Failed to open settings: {}", e))?;
    }
    Ok(())
}

#[command]
async fn take_screenshot(app: AppHandle) -> Result<String, String> {
    use std::time::{SystemTime, UNIX_EPOCH};

    // Generate filename with timestamp hash
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_err(|e| format!("Time error: {}", e))?
        .as_millis();
    let filename = format!("{:x}.png", timestamp);

    // Get screenshots directory and create if needed
    let screenshots_dir = get_screenshots_dir()?;
    std::fs::create_dir_all(&screenshots_dir)
        .map_err(|e| format!("Failed to create screenshots directory: {}", e))?;

    let filepath = screenshots_dir.join(&filename);

    // Use native screencapture on macOS (fast, captures all windows like cmd+shift+4)
    #[cfg(target_os = "macos")]
    {
        // Get display index from overlay window (for multi-monitor support)
        let display_index = if let Some(window) = app.get_webview_window("overlay") {
            if let Ok(Some(monitor)) = window.current_monitor() {
                if let Ok(monitors) = window.available_monitors() {
                    monitors
                        .iter()
                        .position(|m| m.name() == monitor.name())
                        .map(|i| i + 1)
                        .unwrap_or(1)
                } else {
                    1
                }
            } else {
                1
            }
        } else {
            1
        };

        let output = std::process::Command::new("screencapture")
            .arg("-x") // no sound
            .arg("-D")
            .arg(display_index.to_string())
            .arg(&filepath)
            .output()
            .map_err(|e| format!("Failed to run screencapture: {}", e))?;

        if !output.status.success() {
            let stderr = String::from_utf8_lossy(&output.stderr);
            if stderr.contains("could not create image") {
                return Err("Screen recording permission required. Go to System Settings > Privacy & Security > Screen Recording and enable Oto Pure.".to_string());
            }
            return Err(format!("screencapture failed: {}", stderr));
        }
    }

    #[cfg(target_os = "windows")]
    {
        use windows::Win32::Graphics::Gdi::*;
        use windows::Win32::UI::WindowsAndMessaging::*;

        // Get monitor bounds from overlay window
        let (left, top, width, height) = if let Some(window) = app.get_webview_window("overlay") {
            if let Ok(Some(monitor)) = window.current_monitor() {
                let pos = monitor.position();
                let size = monitor.size();
                (pos.x, pos.y, size.width as i32, size.height as i32)
            } else {
                // Fallback to primary screen
                unsafe {
                    (
                        0,
                        0,
                        GetSystemMetrics(SM_CXSCREEN),
                        GetSystemMetrics(SM_CYSCREEN),
                    )
                }
            }
        } else {
            // Fallback to primary screen
            unsafe {
                (
                    0,
                    0,
                    GetSystemMetrics(SM_CXSCREEN),
                    GetSystemMetrics(SM_CYSCREEN),
                )
            }
        };

        unsafe {
            // Get desktop DC
            let screen_dc = GetDC(None);
            if screen_dc.is_invalid() {
                return Err("Failed to get screen DC".to_string());
            }

            // Create compatible DC and bitmap
            let mem_dc = CreateCompatibleDC(screen_dc);
            if mem_dc.is_invalid() {
                ReleaseDC(None, screen_dc);
                return Err("Failed to create compatible DC".to_string());
            }

            let bitmap = CreateCompatibleBitmap(screen_dc, width, height);
            if bitmap.is_invalid() {
                DeleteDC(mem_dc);
                ReleaseDC(None, screen_dc);
                return Err("Failed to create bitmap".to_string());
            }

            // Select bitmap into DC and copy screen from the correct monitor
            let old_bitmap = SelectObject(mem_dc, bitmap);
            BitBlt(mem_dc, 0, 0, width, height, screen_dc, left, top, SRCCOPY)
                .map_err(|e| format!("BitBlt failed: {}", e))?;

            // Prepare bitmap info for GetDIBits
            let mut bmi = BITMAPINFO {
                bmiHeader: BITMAPINFOHEADER {
                    biSize: std::mem::size_of::<BITMAPINFOHEADER>() as u32,
                    biWidth: width,
                    biHeight: -height, // Negative for top-down
                    biPlanes: 1,
                    biBitCount: 32,
                    biCompression: BI_RGB.0,
                    biSizeImage: 0,
                    biXPelsPerMeter: 0,
                    biYPelsPerMeter: 0,
                    biClrUsed: 0,
                    biClrImportant: 0,
                },
                bmiColors: [RGBQUAD::default()],
            };

            // Allocate buffer and get pixels
            let mut pixels: Vec<u8> = vec![0; (width * height * 4) as usize];
            GetDIBits(
                mem_dc,
                bitmap,
                0,
                height as u32,
                Some(pixels.as_mut_ptr() as *mut _),
                &mut bmi,
                DIB_RGB_COLORS,
            );

            // Cleanup GDI objects
            SelectObject(mem_dc, old_bitmap);
            DeleteObject(bitmap);
            DeleteDC(mem_dc);
            ReleaseDC(None, screen_dc);

            // Convert BGRA to RGBA
            for chunk in pixels.chunks_exact_mut(4) {
                chunk.swap(0, 2); // Swap B and R
            }

            // Save using image crate
            let img = image::RgbaImage::from_raw(width as u32, height as u32, pixels)
                .ok_or("Failed to create image from pixels")?;
            img.save(&filepath)
                .map_err(|e| format!("Failed to save screenshot: {}", e))?;
        }
    }

    #[cfg(target_os = "linux")]
    {
        // Check if running in WSL
        let is_wsl = std::fs::read_to_string("/proc/version")
            .map(|v| v.to_lowercase().contains("microsoft") || v.to_lowercase().contains("wsl"))
            .unwrap_or(false);

        if is_wsl {
            // In WSL, use PowerShell to capture Windows desktop
            // Save to Windows temp first, then copy to WSL location
            let temp_filename = format!("oto_screenshot_{}.png", std::process::id());
            let ps_script = format!(
                "Add-Type -AssemblyName System.Windows.Forms; \
                 $screen = [System.Windows.Forms.Screen]::PrimaryScreen; \
                 $bitmap = New-Object System.Drawing.Bitmap($screen.Bounds.Width, $screen.Bounds.Height); \
                 $graphics = [System.Drawing.Graphics]::FromImage($bitmap); \
                 $graphics.CopyFromScreen($screen.Bounds.Location, [System.Drawing.Point]::Empty, $screen.Bounds.Size); \
                 $bitmap.Save(\"$env:TEMP\\\\{}\");",
                temp_filename
            );
            let output = std::process::Command::new("powershell.exe")
                .args(["-Command", &ps_script])
                .output()
                .map_err(|e| format!("Failed to capture screenshot via PowerShell: {}", e))?;

            if !output.status.success() {
                return Err(format!(
                    "PowerShell screenshot failed: {}",
                    String::from_utf8_lossy(&output.stderr)
                ));
            }

            // Get Windows username and copy from Windows temp to WSL location
            let win_user = std::env::var("USER").unwrap_or_else(|_| "user".to_string());
            let temp_path = format!(
                "/mnt/c/Users/{}/AppData/Local/Temp/{}",
                win_user, temp_filename
            );

            // Copy from Windows temp to final location
            std::fs::copy(&temp_path, &filepath).map_err(|e| {
                format!(
                    "Failed to copy screenshot from temp: {} (temp: {})",
                    e, temp_path
                )
            })?;

            // Clean up temp file
            let _ = std::fs::remove_file(&temp_path);
        } else {
            // Native Linux: use gnome-screenshot or scrot
            let output = std::process::Command::new("gnome-screenshot")
                .arg("-f")
                .arg(&filepath)
                .output();

            if output.is_err() || !output.as_ref().unwrap().status.success() {
                std::process::Command::new("scrot")
                    .arg(&filepath)
                    .output()
                    .map_err(|e| {
                        format!(
                            "Failed to capture screenshot (install gnome-screenshot or scrot): {}",
                            e
                        )
                    })?;
            }
        }
    }

    println!("[screenshot] Saved to: {:?}", filepath);

    Ok(filepath.to_string_lossy().to_string())
}

#[command]
async fn open_screenshots_folder() -> Result<(), String> {
    let screenshots_dir = get_screenshots_dir()?;

    // Create directory if it doesn't exist
    std::fs::create_dir_all(&screenshots_dir)
        .map_err(|e| format!("Failed to create screenshots directory: {}", e))?;

    // Open in file manager
    #[cfg(target_os = "macos")]
    {
        std::process::Command::new("open")
            .arg(&screenshots_dir)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    #[cfg(target_os = "windows")]
    {
        std::process::Command::new("explorer")
            .arg(&screenshots_dir)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    #[cfg(target_os = "linux")]
    {
        std::process::Command::new("xdg-open")
            .arg(&screenshots_dir)
            .spawn()
            .map_err(|e| format!("Failed to open folder: {}", e))?;
    }

    Ok(())
}

// ============ Main ============

fn main() {
    tauri::Builder::default()
        .manage(AppState::default())
        .setup(|app| {
            // Create tray menu
            let toggle_item =
                MenuItem::with_id(app, "toggle", "Show Character", true, None::<&str>)?;
            let settings_item = MenuItem::with_id(app, "settings", "Settings", true, None::<&str>)?;
            let quit_item = MenuItem::with_id(app, "quit", "Quit", true, None::<&str>)?;

            // Store toggle item in state for later text updates
            let state = app.state::<AppState>();
            *state.toggle_menu_item.lock().unwrap() = Some(toggle_item.clone());

            let menu = Menu::with_items(app, &[&toggle_item, &settings_item, &quit_item])?;

            // Create tray icon
            let _tray = TrayIconBuilder::new()
                .icon(app.default_window_icon().unwrap().clone())
                .menu(&menu)
                .show_menu_on_left_click(false)
                .on_menu_event(move |_app, event| {
                    match event.id.as_ref() {
                        "toggle" => {
                            toggle_overlay_sync(_app);
                        }
                        "chat_history" => {
                            // Show overlay and emit event to open history modal
                            if let Some(window) = _app.get_webview_window("overlay") {
                                let _ = window.show();
                                let _ = window.set_focus();
                            }
                            let _ = _app.emit("show-chat-history", ());
                        }
                        "settings" => {
                            // Show main window (for API key entry, etc.)
                            if let Some(window) = _app.get_webview_window("main") {
                                let _ = window.show();
                                let _ = window.set_focus();
                                let _ = _app.emit(
                                    "main-window-visibility-changed",
                                    json!({ "visible": true }),
                                );
                            }
                        }
                        "screenshots" => {
                            // Open screenshots folder
                            std::thread::spawn(|| {
                                let _ = tauri::async_runtime::block_on(open_screenshots_folder());
                            });
                        }
                        "clear_data" => {
                            if let Err(e) = clear_app_data() {
                                eprintln!("Error clearing app data: {}", e);
                            }
                        }
                        "quit" => {
                            std::process::exit(0);
                        }
                        _ => {}
                    }
                })
                .on_tray_icon_event(|tray, event| {
                    if let tauri::tray::TrayIconEvent::Click {
                        button: MouseButton::Left,
                        button_state: MouseButtonState::Up,
                        ..
                    } = event
                    {
                        toggle_overlay_sync(tray.app_handle());
                    }
                })
                .build(app)?;

            // Register global shortcut (Option+Space on macOS, Super+Space on Linux/WSL, Alt+Space on Windows)
            #[cfg(target_os = "linux")]
            let shortcut = Shortcut::new(Some(Modifiers::SUPER), Code::Space);
            #[cfg(not(target_os = "linux"))]
            let shortcut = Shortcut::new(Some(Modifiers::ALT), Code::Space);
            app.global_shortcut().register(shortcut)?;

            Ok(())
        })
        .on_window_event(|window, event| {
            if window.label() == "main" {
                if let tauri::WindowEvent::CloseRequested { api, .. } = event {
                    // Prevent the window from actually closing - just hide it
                    api.prevent_close();
                    let _ = window.hide();
                    let _ = window.app_handle().emit(
                        "main-window-visibility-changed",
                        serde_json::json!({ "visible": false }),
                    );
                }
            }
        })
        .plugin(tauri_plugin_dialog::init())
        .plugin(
            tauri_plugin_global_shortcut::Builder::new()
                .with_handler(|app, shortcut, event| {
                    if event.state() == ShortcutState::Pressed
                        && (shortcut.matches(Modifiers::ALT, Code::Space)
                            || shortcut.matches(Modifiers::SUPER, Code::Space))
                    {
                        // Show overlay if hidden
                        let is_visible = {
                            let state = app.state::<AppState>();
                            let visible = *state.overlay_visible.lock().unwrap();
                            visible
                        };
                        if !is_visible {
                            toggle_overlay_sync(app);
                        }
                        // Focus the overlay window so keyboard input works
                        if let Some(window) = app.get_webview_window("overlay") {
                            let _ = window.set_focus();
                        }
                        let _ = app.emit("toggle-textbox", ());
                    }
                })
                .build(),
        )
        .invoke_handler(tauri::generate_handler![
            init_app,
            get_paths,
            read_file_as_text,
            read_file_as_bytes,
            is_initialized,
            show_overlay,
            hide_overlay,
            toggle_overlay,
            get_overlay_visible,
            hide_main_window,
            show_main_window,
            toggle_main_window,
            is_main_window_visible,
            start_device_listening,
            check_screen_permission,
            open_screen_recording_settings,
            take_screenshot,
            open_screenshots_folder,
            save_api_key,
            get_api_key,
            has_api_key,
            save_system_prompt,
            get_system_prompt,
            save_character_prompt,
            get_character_prompt,
            save_deep_research_prompt,
            get_deep_research_prompt,
            save_dialogue_prompt,
            get_dialogue_prompt,
            send_chat_message,
            get_chat_history,
            clear_chat_history,
            trigger_deep_research,
            clear_all_data,
            generate_texture,
            get_texture_paths,
            reload_character,
            get_texture_versions,
            apply_texture_version,
            delete_texture_version,
            save_hitbox,
            load_hitbox,
            clear_hitbox,
        ])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
