# ADR-0009: コンパクションは旧id昇順リマップ(再ソートなし)

日付: 2026-06-11 / 状態: 採用済み

## 決定

tombstone行とプール内の死バイトはコンパクションで回収する。生存エントリは旧id昇順で再番号付けする — 相対順序が保存されるため、(key, id)順の全構造(perm_name、FRN索引)は filter+remap のO(n)コピーで引き継がれ、再ソートしない。volumeスレッドがバッチ適用毎に閾値判定する: `len≥100k かつ(tombstone_ratio>12.5% または dead_name_bytes>32MiB)`。

## 根拠

- tombstone行とrename放棄の名前バイトは無限に蓄積し、B/entry RAMゲートへのスローリーク(従来はフルリスキャンでしか回収できない)
- 旧id昇順リマップなら生存エントリの相対順序が保存され、ソート済み構造はバイト等価にfilter+remapできる(O(n)、ソートコストゼロ)
- 判定入力は dead_name_bytes 可観測性(IndexStats.pool_garbage_ratio)。閾値は実ボリューム観測を前提に設定

## 影響

- コピー構築はread guard下で行う(クエリ並走可。書き手はvolumeスレッド1本のみ)。swapは `install_index` 経由でµs級write lock+structural generation bump
- コンパクション毎に開いている結果ハンドルはハードSTALE(`FMF_E_STALE`)→UIが同一クエリを自動再発行(既存機構)
- 死んだディレクトリの子はrootへ付け替え(push_rawのorphan方針と同一)
- 防御の世代チェック失敗は `compaction_aborts` カウンタ+コピー破棄(単一書き手不変条件の破れ検知。黙らない)

## 再検討トリガ

- `compaction_aborts > 0` の観測(単一書き手不変条件の見直し)
- 閾値が実運用でコンパクション頻発または回収不足を示した場合(閾値再調整)
