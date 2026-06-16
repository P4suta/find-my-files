# ADR-0024: 非昇格スコープ索引モード(フォルダ走査 + ReadDirectoryChangesW)

日付: 2026-06-16 / 状態: 採用済み

## 決定

管理者権限なし(asInvoker)で、ユーザーが指定したルート集合だけを索引する**スコープモード**を追加する。索引源は MFT/USN ではなく**フォルダ走査(ディレクトリ列挙)+ ReadDirectoryChangesW**。サービスを使わず UIプロセス内 in-proc で動き、索引は `%LOCALAPPDATA%\find-my-files\`(ユーザー単位・独自 `.writer.lock`)に置く。

ADR-0018 の「シーム2本上限・追加ポート禁止」は破らない。スコープモードは既存2シームの**第2実装**として収める:

- スナップショット作成側(シーム外のスキャン): `mft::scan_volume` の隣に `scan::walk::walk_scan` を置き、`worker.rs` の establish 地点1箇所だけ分岐する。
- `SnapshotStore`: `WinSnapshotStore` をそのまま再利用(パスが `%LOCALAPPDATA%` になるだけ)。
- `JournalSource`: USN の `WinJournalSource` に対し `WatcherJournalSource`(ReadDirectoryChangesW)を第2実装として追加。

これは ADR-0001 の「フォルダ走査索引はしない/ボリューム単位のみ」を**この一点に限って改訂する**(下記「ADR-0001 との関係」)。

## 根拠

- ターゲット(検索を多用するビジネスマン)の会社PCは IT部門が昇格を禁止する。MFT直読みも USN もサービス導入も全て管理者必須で、製品ごと締め出されていた。非昇格で実データを索引する唯一の枯れた経路がフォルダ走査 + ReadDirectoryChangesW。
- 全C:は捨てるが、ナレッジワーカーが実際に触るルート(プロファイル / OneDrive・SharePoint同期 / マップドライブ / 案件フォルダ = 数万〜数十万件)に限れば、走査は秒で終わり、鮮度も維持できる。
- **合成FRN で索引フォーマットを変えずに済む**: 索引のアイデンティティ鍵は `Frn::record()` = 下位48bit(`index/mod.rs`)で、生存性で NTFS レコード再利用を解決する設計。実FRNは不要。スコープモードは `frn = xxhash64(折り畳み絶対パス)` を使う。絶対パスはグローバルに一意なので下位48bitもルート跨ぎで一意になり、別途 root id を持たせる必要はない(`record()` は上位bitを捨てるため root id を上位に置いても lookup には効かない)。watcher は変更パスから同じハッシュを無状態で再計算できる。
- マルチルートは形式変更なしで表現できる: 各ルートを ROOT の子(name = 絶対ベースパス)として push すれば、既存のパス再構成がそのまま正しい絶対パスを返す。
- query 経路は同一 `VolumeIndex` ゆえ無変更で、p99 死守線(3文字以上で <10ms 体感)に影響しない。

## 影響

- **改訂は最小限**: 「ファイル名のみ索引」の核は維持。content索引・プロパティ/タグ索引・プレビューは依然不採用(ADR-0001)。スコープモードでも保持するのはファイル名・サイズ・更新日時・属性のみ。
- 合成FRN は rename でパスが変わると別IDになる。watcher は ReadDirectoryChangesW の old/new パス対を `delete(hash(old)) + create(hash(new))` に翻訳して `apply_batch` を再利用する。ディレクトリ rename は新パスの subtree 再walk(稀・bounded、dir-rename と同じ accepted-limitation 類)。
- 下位48bit衝突は確率的(数十万件で <0.1%)。衝突は1ファイルが他を遮蔽するのみで、再walk(手動再索引/定期再walk/journal-gone 相当)で自己修復する。
- 鮮度はネットワーク/クラウド(OneDrive placeholder)で ReadDirectoryChangesW が取りこぼしうるため、**定期再walk** を安全網にする。placeholder は列挙メタのみで索引し、絶対に hydrate しない(データ課金・性能事故の回避)。
- 既存の昇格モード(サービス/inproc・全ボリューム)とは排他ではなく、サービス不在 & 非昇格時の選択肢。設定が無ければ従来どおり setup 画面へ誘導する。

## ADR-0001 との関係

ADR-0001 の影響節「ボリューム単位のみ・フォルダ走査索引はしない」を本ADRが上書きする。ADR-0001 が守る本丸(ファイル名のみ索引・content索引除外・スコープクリープ防止)は不変。本ADRが許可するのは「非昇格で読めるルートをフォルダ走査で索引する」一点のみで、FTP/HTTP/ETPサーバ・FAT/exFAT・ReFS・クロスプラットフォーム化は引き続き不採用。

## 再検討トリガ

- スコープモードの cold-start(数十万件のwalk)が体感を損ねる水準まで遅い場合 → rayon 並列ルートwalk・`NtQueryDirectoryFile` バルク化を再評価(Phase 3)。
- 下位48bit衝突が実索引で観測される頻度が無視できない場合(計測カウンタで監視)→ レコード採番方式の再設計。
- ReadDirectoryChangesW の取りこぼしが定期再walk で吸収しきれない場合 → per-root 溢れ回復の精緻化。
