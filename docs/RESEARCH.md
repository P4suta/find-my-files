# 裏取り済み技術事実(2026-06-10 調査、一次情報確認済み)

設計判断はこのファイルを前提とする。出典は各項目末尾。

## NTFS / MFT / USN ジャーナル

- **FSCTL_ENUM_USN_DATA**(DeviceIoControl、winioctl.h、文書化済み)はMFTレコードを列挙する公式API。`MFT_ENUM_DATA_V0/V1` を入力に `StartFileReferenceNumber=0` から繰り返し呼ぶ。返る `USN_RECORD_V2` には FRN・親FRN・ファイル名・FileAttributes はあるが**ファイルサイズとタイムスタンプが無い**(TimeStampはジャーナル記録時刻)。サイズ・日付込みの索引には生$MFT読み($STANDARD_INFORMATION/$FILE_NAME/$DATA)か、ファイル毎の追加問い合わせが必要。
  https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_enum_usn_data
  https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ns-winioctl-usn_record_v2
- **増分監視**: `FSCTL_QUERY_USN_JOURNAL` で UsnJournalID/NextUsn 取得 → `FSCTL_READ_USN_JOURNAL`(`READ_USN_JOURNAL_DATA_V0`、`BytesToWaitFor>0` でブロッキング購読可)。永続化すべき状態は **UsnJournalID + 最終処理USN** のペア。ジャーナルはOSが維持するためアプリ停止中の変更も追いつける。
  https://learn.microsoft.com/en-us/windows/win32/api/winioctl/ni-winioctl-fsctl_read_usn_journal
- **エラーフォールバック(定石)**: `ERROR_JOURNAL_NOT_ACTIVE` → `FSCTL_CREATE_USN_JOURNAL`(管理者必須)で作成。`ERROR_JOURNAL_DELETE_IN_PROGRESS`(削除は再起動を跨いで継続)。保存USNがFirstUsnより古い → `ERROR_JOURNAL_ENTRY_DELETED`。これらとJournalID不一致は**フルリスキャンへフォールバック**。
  https://learn.microsoft.com/en-us/windows/win32/fileio/creating-modifying-and-deleting-a-change-journal
- **FRN→パス**: USNレコードにパス文字列は無い。全ディレクトリの FRN→(名前, 親FRN) マップを保持し、ルート(NTFSはMFTレコード5固定)まで親チェーンを辿って遅延構築。フォルダrename/moveは当該1レコードのみ更新され子のレコードは発生しない。FRNはNTFSで64bit(下位48bit=レコード番号+上位16bit=シーケンス)。ReFSは128bit(USN_RECORD_V3)— MVP対象外だがID型設計で考慮。
- **権限**: ボリュームハンドル(`\\.\C:`)のオープンは管理者必須(CreateFile公式Remarks「The caller must have administrative privileges」)。非公開の `FSCTL_READ_UNPRIVILEGED_USN_JOURNAL` で非昇格のジャーナル読みは可能だが未文書・ENUM相当が無いため初回スキャンは昇格必須。
  https://learn.microsoft.com/en-us/windows/win32/api/fileapi/nf-fileapi-createfilew
- **ハードリンク**: 1つのMFTレコード内の複数$FILE_NAME属性。USNレコードのファイル名は「通常最初のリンク名」のみ(開発者void本人の説明)。Everything 1.5ですら削除リンク未追跡。→ MVPは「FRNごとに代表名1つ」(Everything 1.3水準)。
  https://www.voidtools.com/forum/viewtopic.php?t=6189
- **シンボリックリンク/ジャンクション**: Everythingは辿らない(循環の照合コスト)。リパースポイント自体を1エントリとして索引。同方式を採る。

## Everything の実装(開発者本人のフォーラム発言で確認)

- **検索方式**: 転置インデックスではなく「UTF-8ファイル名リスト+サイズ+更新日時+親フォルダへのポインタ」を保持し、**マルチスレッド最適化strstrの線形スキャン**。検索式はバイトコードにコンパイルして実行。
  https://www.voidtools.com/forum/viewtopic.php?t=9463
- **fast sort**: 名前/サイズ/更新日時等の事前ソート済み索引を保持し、列ヘッダクリックはソートせず既ソート順を提示するだけ。fast sort無効プロパティのソートは遅い(公式明記)。
  https://www.voidtools.com/support/everything/indexes/
- **性能基準値(公式FAQ)**: 25万ファイル≈5秒・35MB RAM / 100万ファイル≈1分・100MB RAM・45MBディスク(**≈100バイト/ファイル**)。MFT読み自体は約1秒/ドライブ。NTFS索引には管理者権限またはEverything Service必須。
  https://www.voidtools.com/faq/
- **検索構文のコア**(公式 + HN実利用調査): substringデフォルト、space=AND、`|`=OR、`!`=NOT、`""`=フレーズ、`*?`(ファイル名全体マッチ)、`ext:` `path:` `size:` `dm:`(範囲 `a..b` `>x`)。regex:/content:はニッチ。content:は公式も「extremely slow」。
  https://www.voidtools.com/support/everything/searching/

## 競合・先行(2026-06時点)

- 「Rustエンジン+WinUI 3ネイティブ+真のFOSS」は空白地帯。最有力競合 omni-search(Eul45、2026-02開始、517 stars、MIT)はTauri v2+React+C++、requireAdministrator方式。
- 歴代FOSSクローンは全停滞: Orange(Rust/Tauri/Tantivy、walk方式でMFT不使用、2023-10停止)、FastFileSearch(2016)、Indexer++(2019)、SwiftSearch(実はCC BY-NC=非FOSS、2019)。
- EverythingToolbar(14.1k stars)= 「エンジンは本家のままUIだけモダンに」で大支持 → **UI刷新自体に大きな需要**。
- Everything本家: 1.5が約5年alphaの末2026-05-14にbeta移行。ソースは非公開(License.txt文面はMIT形式だがコード未配布)。周辺ツール(ES/etp_server等)は2025年にOSS化されたが本体はクローズドのまま。

## 実C:の名前・サイズ統計(2026-06-11、`fmf stats C: --name-stats` 実測、1,268,450件)

プール/カラムレイアウト判断の一次データ(エンジン本体と同じWTF-8/fold規則で計測):

- **fold同一(lower==orig バイト一致)= 73.2%** — lower_pool への重複格納の約3/4は原文と同一バイト
- ユニーク名 53.2% / folded後ユニーク 53.0%(大文字小文字違いの同名はほぼ無い)
- 名前長(WTF-8バイト): 平均 29.7 / p50 18 / p90 90 / p99 110 / max 171
- **4GiB超ファイル = 10件(0.0008%)** → size列 u32+オーバーフロー側テーブルは余裕で成立
- 原文オーバーフロー化の予測節約: 全entry u32カラム方式 **−17.7B/entry**、ソート済みペア方式 −19.6B/entry(後者はresidual毎の二分探索コストが乗るため、p99リスク回避で前者を採用)
- 参考(計測時点の収支): 計上126.3B/entry、WS 142B/entry(172.1MiB)

## Rust crates(実在・成熟度確認済み)

- `ntfs-reader` 0.4.5(MIT/Apache-2.0、2026-03更新): 生$MFT全レコードスキャン(README記載ベンチ: Vec Cache 3.756s/HashMap 4.981s/No Cache 12.3s、環境記載なし)。FileInfoで name/path/size/created/modified。**全ハードリンク名は取れない(代表名1つ)**。
- `usn-journal-rs`(wangfu91、MIT、2026-05更新): MFT列挙+USN監視+FRNパス解決。参考実装として読む(依存はしない方針)。
- `windows-sys` 0.61: FSCTL定数・MFT_ENUM_DATA・USN_RECORD等完備。USNラッパーは自前実装(~200行)。
- `memchr`(memmem::Finder=SIMD substring)、`rayon`、`parking_lot`、`thiserror`、`tracing`、`xxhash-rust`。

## WinUI 3(Windows App SDK)

- **データ仮想化**: 件数既知のランダムアクセスは **非ジェネリック`IList` + `INotifyCollectionChanged` + `IItemsRangeInfo` + プレースホルダ**。現行WASDKでサポート明記(MS Learn 2026-03更新)。`IList<T>`のみでは動かない(#1809)。`ISupportIncrementalLoading`はクラッシュ報告(#6883)あり回避。ItemsView/ItemsRepeaterは両IF非対応。ItemsPanelをItemsStackPanel以外にすると仮想化が無効化。
  https://learn.microsoft.com/en-us/windows/apps/develop/performance/listview-and-gridview-data-optimization
- **トレイ/ホットキー**: ネイティブサポート無し。H.NotifyIcon.WinUI + 自前 RegisterHotKey + HWND_MESSAGE隠しウィンドウ(WM_HOTKEY)。
- **DPI**: WinUI 3テンプレートは既定Per-Monitor V2。本家Everythingはsystem DPI止まり(開発者が2024-12にPMv2はTODOと明言)→ 構造的差別化点。
- **MSIX×requireAdministrator は筋が悪い**(allowElevation等の制約、Store審査ほぼ却下)→ unpackaged + self-contained 配布。
- **昇格プロセスの既知制約**: Explorerからの D&D 不可(UIPI)。昇格プロセスから直接ShellExecuteすると関連付けアプリも昇格起動 → `explorer.exe "<path>"` 経由で脱昇格(定石)。
- WASDK 1.6+でNative AOT対応(公式サンプルで起動約50%短縮)。ただし「即時起動」体験はトレイ常駐+ホットキーで担保するのが本筋。

## セキュリティ(v2向けメモ)

特権インデクサ→非特権UIの構成は、ACL上見えないはずのファイル名・パスを露出させる情報漏洩を内包(Everything自身の既知問題。ETP/HTTPサーバでは実際に事件化しLite版でIPC削除)。v2では pipe DACL で「同一ユーザーのみ」を保証+脅威モデルをドキュメント化。MVPは昇格プロセス内完結なので非該当。
