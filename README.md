# Doge-Code

このリポジトリは Doge-Code CLI/MCP エージェントの実装です。コード探索の起点となる `search_repomap` ツールに新しいレスポンス制御オプションが加わりました。

## search_repomap チートシート
- `result_density`: 既定の `"compact"` ではスニペットを返さず、1 ファイルあたり 5 シンボルに圧縮。詳細が欲しいファイルだけ `"full"` に切り替えるとコンテキスト節約になります。
- `response_budget_chars`: 「5,000 文字以内」のように上限を渡すと、limit／シンボル数／スニペット長を自動で削り、結果が大きくなりすぎるのを防ぎます。予算内に収まらない場合は `warnings` と `next_cursor` で続きが取得できます。
- `cursor` / `page_size`: ソート済み結果をページ分割できます。`cursor` は 0 ベースの次の位置、`page_size` は取得件数です。`next_cursor` が `Some(x)` なら次ページを同じクエリ + `cursor=x` で取得してください。
- レスポンスは `SearchRepomapResponse` で返り、`results`（従来の `RepomapSearchResult` 群）に加えて `warnings` と `applied_budget`（実際に適用された制限の概要）が含まれます。

これらを組み合わせることで、LLM のコンテキストを圧迫せずに最大効果のコード探索が可能です。

## ファイル系ツールの軽量モード
- `fs_read`: 既定モードは `mode="summary"` で 400 行 & 6,000 文字までを返し、残りは `next_cursor` で追跡できます。全文が必要な時だけ `mode="full"` や `page_size`/`cursor` を指定してください。
- `fs_read_many_files`: `paths` で解決されたファイル群を `mode="summary"` では 1 ページ 5 件・各ファイル 40 行までで返し、`response_budget_chars` を超えそうな場合は自動的に `warnings` + `next_cursor` を返します。
- `fs_list`: ディレクトリ一覧も `FsListResponse` で返り、`entries` には `path` と `is_dir` だけを載せるためコンパクトです。`cursor`/`page_size`/`response_budget_chars` を活用して深い木構造を段階的に取得してください。
