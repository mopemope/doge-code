use crate::tui::state::{LlmResponseSegment, TuiApp}; // TuiAppとLlmResponseSegmentをインポート
use regex::Regex;

// TuiAppにLLMレスポンス処理のロジックを実装
impl TuiApp {
    // 新規: LLMストリーミングトークンを構造化して処理するメソッド (簡易版)
    // トークンはバッファに蓄積され、ストリーム完了時に一括解析される。
    // #[allow(dead_code)]
    pub fn append_stream_token_structured(&mut self, s: &str) {
        // 受信したトークンを解析バッファに追加
        self.llm_parsing_buffer.push_str(s);
    }

    // 既存のメソッド（左マージン付き）は一時的に残すが、新しい実装で置き換える
    #[allow(dead_code)]
    pub fn append_stream_token(&mut self, s: &str) {
        // 左マージン付きでストリーミングトークンを追加
        self.append_stream_token_with_margin(s, 2); // 2文字分のマージン
    }

    // 新規: 左マージン付きでストリーミングトークンを追加する内部メソッド
    fn append_stream_token_with_margin(&mut self, s: &str, margin: usize) {
        let margin_str = " ".repeat(margin); // マージン用のスペース文字列を作成

        // Normalize incoming token: split by '\n' and append as multiple logical lines if needed.
        let parts: Vec<&str> = s.split('\n').collect();
        if parts.is_empty() {
            return;
        }

        // 最初のパートは、既存の最後の行に追加
        if let Some(last) = self.log.last_mut() {
            last.push_str(parts[0]);
        }

        // 2番目以降のパートは新しい行として追加（マージン付き）
        for seg in parts.iter().skip(1) {
            // 空行でない場合、または空行でもマージンを適用したい場合はこの条件を調整
            // ここでは空行もマージン付きで追加する
            let line_with_margin = format!("{margin_str}{seg}");
            self.log.push(line_with_margin);
        }
    }

    // 新規: llm_parsing_buffer をパースして構造化セグメントを作成し、self.log に追加する
    // 新規: llm_parsing_buffer をパースして構造化セグメントを作成し、self.log に追加する
    pub(crate) fn finalize_and_append_llm_response(&mut self) {
        // 解析バッファを消費
        let buffer = std::mem::take(&mut self.llm_parsing_buffer);
        if buffer.is_empty() {
            // バッファが空なら、current_llm_response もクリアして終了
            self.current_llm_response = None;
            return;
        }

        // current_llm_response を take() で所有権ごと取得し、後で処理するセグメントリストを構築
        // これにより、self.current_llm_response への借用をすぐに解除できる
        let mut response_segments = self.current_llm_response.take().unwrap_or_default();

        // 正規表現でコードブロックを抽出
        // (?s) フラグは、`.` が改行にもマッチするようにする（複数行マッチ）
        // `?` により非貪欲マッチ（最初に見つかった ``` で終了）
        let re = Regex::new(r"(?s)```(\w*)\n(.*?)```").unwrap();
        let mut last_end = 0;

        for cap in re.captures_iter(&buffer) {
            let whole_match = cap.get(0).unwrap();
            let lang = cap.get(1).map_or("", |m| m.as_str());
            let code_content = cap.get(2).map_or("", |m| m.as_str());

            // コードブロックより前のテキスト（Textセグメント）を追加
            if whole_match.start() > last_end {
                let text_content = &buffer[last_end..whole_match.start()];
                if !text_content.is_empty() {
                    response_segments.push(LlmResponseSegment::Text {
                        content: text_content.to_string(),
                    });
                }
            }

            // コードブロック（CodeBlockセグメント）を追加
            response_segments.push(LlmResponseSegment::CodeBlock {
                language: lang.to_string(),
                content: code_content.to_string(),
            });

            last_end = whole_match.end();
        }

        // 最後のコードブロックより後のテキスト（Textセグメント）を追加
        if last_end < buffer.len() {
            let text_content = &buffer[last_end..];
            if !text_content.is_empty() {
                response_segments.push(LlmResponseSegment::Text {
                    content: text_content.to_string(),
                });
            }
        }

        // この時点で、response_segments にはすべての構造化されたセグメントが含まれている
        // self.current_llm_response は None になっている（take() したため）
        // したがって、これ以降の self の他の部分（例: self.log）への可変借用は安全に行える

        // response_segments (Vec<LlmResponseSegment>) をフラットな String Vec に変換して self.log に追加
        for segment in response_segments {
            match segment {
                LlmResponseSegment::Text { content } => {
                    // テキストコンテンツを、改行で分割してログに追加
                    // 既存の append_stream_token_with_margin のロジックを再利用
                    self.append_stream_token_with_margin(&content, 2); // テキストにもマージンを適用
                }
                LlmResponseSegment::CodeBlock { language, content } => {
                    // コードブロックの開始を示す識別子行を追加
                    self.log.push(format!(" [CodeBlockStart({language})]"));
                    // コードコンテンツを、改行で分割してログに追加（マージン付き）
                    self.append_stream_token_with_margin(&content, 4); // コードブロックにはより広いマージン
                    // コードブロックの終了を示す識別子行を追加
                    self.log.push(" [CodeBlockEnd]".to_string());
                }
            }
        }

        // current_llm_response は処理後 None に設定されている（take() したため）
        // 特に再設定の必要なし
    }
}
