# Doge-Code

このリポジトリは Doge-Code CLI/MCP エージェントの実装です。コード探索の起点となる `search_repomap` ツールに新しいレスポンス制御オプションが加わりました。

## search_repomap チートシート
- `result_density`: 既定の `"compact"` ではスニペットを返さず、1 ファイルあたり 5 シンボルに圧縮。詳細が欲しいファイルだけ `"full"` に切り替えるとコンテキスト節約になります。
- `response_budget_chars`: 「5,000 文字以内」のように上限を渡すと、limit／シンボル数／スニペット長を自動で削り、結果が大きくなりすぎるのを防ぎます。予算内に収まらない場合は `warnings` と `next_cursor` で続きが取得できます。
- `cursor` / `page_size`: ソート済み結果をページ分割できます。`cursor` は 0 ベースの次の位置、`page_size` は取得件数です。`next_cursor` が `Some(x)` なら次ページを同じクエリ + `cursor=x` で取得してください。
- レスポンスは `SearchRepomapResponse` で返り、`results`（従来の `RepomapSearchResult` 群）に加えて `warnings` と `applied_budget`（実際に適用された制限の概要）が含まれます。

これらを組み合わせることで、LLM のコンテキストを圧迫せずに最大効果のコード探索が可能です。
