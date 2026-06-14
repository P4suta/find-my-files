# ADR-0002: 線形プールスイープ+インクリメンタル検索(trigram転置索引は不採用)

日付: 2026-06-11 / 状態: 採用済み

## 決定

検索はfold済み名前プールの線形スイープ(SIMD memmem、rayon 64kチャンク並列)で行う。直前クエリを証明可能に絞り込む再クエリは `query::refine` が前回ヒット集合のみを再評価する(query/subsume.rs の保守的包含規則)。trigram転置索引は採用しない。

## 根拠

- 合成1M件のコールド3文字クエリは約2.9ms(クエリキャッシュMISS+派生キャッシュwarm、materialize込み)。判定基準「per-volume scan_us p99 > 25ms @1M」を一桁下回る
- posting維持コストはRAM ≤110B/file制約下で +10〜15B/file、加えてUSNバッチ毎の差分維持が必要。見合わない
- インクリメンタル検索はO(前回ヒット数)でスキャンもO(n)マテリアライズも省略

## 影響

- refineの適用は保守的包含規則(同一ソート・単一ANDグループ・needle包含/範囲縮小/フィルタ追加のみ)に限定。正しさはoracleプロパティテスト(refine == fresh search)で担保
- キルスイッチ `FMF_QUERY_CACHE=0`、観測は `QueryTrace.cache`(miss/refine/partial)

## 再検討トリガ(全て満たす場合のみ)

1. キャッシュMISS時のコールド3文字 scan_us p99 > 25ms @1M
2. `fmf stats --trigram-estimate` の実測見積もり ≤15B/file かつ合計 ≤110B/file
3. posting差分維持 ≤2ms/バッチ
4. 1ボリューム400万件超の実需要
