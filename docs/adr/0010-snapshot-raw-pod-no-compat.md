# ADR-0010: スナップショットは生PODダンプ+全検証、後方互換なし

日付: 2026-06-11 / 状態: 採用済み

## 決定

永続化は自前バイナリ **FMFIDX04**: magic+UsnJournalID+最終USN+生カラム列ダンプ+xxhash64。セクションは lower_pool / orig_pool / orig_off / name_off / name_len / parent / size_lo / size-overflow ids+sizes / mtime / frn / flag / perm_name。後方互換は持たない — 版不一致・検証失敗は常にErr→フルリスキャン。

## 根拠

- 実C: 1.27M件で92.4MiB(旧形式128.6MiBから−28%)、restore p50 81ms — restore→ready ≤2sゲートに大幅余裕
- リスキャンは2.0s(ADR-0011)で安価。マイグレーションコードの維持・テストコストに見合わない
- ロード時はchecksumに加え全スライス境界とオーバーフロー対応の構造検証を行う(壊れた入力でpanicせずErr→リスキャン)
- size/mtime順列とFRN索引は非永続(復元時/初回利用時の並列ソート再構築が直列ロードより速い: load_1m −34%、ADR-0005/0006)

## 影響

- フォーマット版上げのたびに各ボリューム1回のフルリスキャン(2s級、昇格必須)を受け入れる
- structural_generationは永続化しない(復元時0)。結果ハンドルはプロセスを跨がないためプロセス内の単調性で十分
- 書き込みはtemp→`MoveFileEx(REPLACE_EXISTING)`。失敗は snapshot_load_failures / snapshot_save_failures カウンタ

## 再検討トリガ

- 初回スキャンが分単位になる規模が主要ターゲットになり、版上げ毎リスキャンの体感コストが問題化した場合
