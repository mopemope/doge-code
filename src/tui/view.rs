// src/tui/view.rs は、TUIのビュー関連のロジックをまとめたファイルです。
// 実際の実装は、event_loop.rs、rendering.rs、llm_response_handler.rs に分割されています。
// このファイルでは、それらのモジュールを再エクスポートするだけです。

// TuiApp構造体とその関連アイテムも再エクスポート
pub use crate::tui::state::TuiApp;
