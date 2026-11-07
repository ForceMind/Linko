# Linko device illustrations

Original transparent hardware illustrations generated for this project with the built-in image_gen tool. They contain no product logos, third-party operating-system wallpapers, live screen pixels or actual device-model claims. They are presentation assets only; names, status and permissions come from the application state.

Exact prompts and SHA256/dimensions are recorded in `docs/design/linko-v1/device-assets.json`. Original generated PNG bytes are preserved. UI rendering uses the alpha bounds as texture coordinates to exclude transparent margins, then caches the upload per egui context. No image generation, network request or screen capture occurs at runtime.
