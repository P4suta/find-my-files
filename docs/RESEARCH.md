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
- **ハードリンク**: 1つのMFTレコード内の複数$FILE_NAME属性。USNレコードのファイル名は通常「最初のリンク名」のみ。→ MVPは「FRNごとに代表名1つ」。
- **シンボリックリンク/ジャンクション**: 辿らない(循環の照合コスト)。リパースポイント自体を1エントリとして索引。

## 検索構文(実利用調査)

- 実利用調査(HN 等)では substring デフォルト、`space`=AND、`|`=OR、`!`=NOT、`""`=フレーズ、`*?`(ファイル名全体マッチ)、`ext:` `path:` `size:` `dm:`(範囲 `a..b` `>x`)が中心。`regex:`/`content:` はニッチで、content 検索は本質的に低速。→ 構文スコープと「ファイル名のみ索引」割り切りの裏付け(ADR-0001)。

## 競合・先行(2026-06時点)

- 「Rustエンジン+WinUI 3ネイティブ+真のFOSS」は空白地帯。最有力競合 omni-search(Eul45、2026-02開始、517 stars、MIT)はTauri v2+React+C++、requireAdministrator方式。
- 歴代FOSSクローンは全停滞: Orange(Rust/Tauri/Tantivy、walk方式でMFT不使用、2023-10停止)、FastFileSearch(2016)、Indexer++(2019)、SwiftSearch(実はCC BY-NC=非FOSS、2019)。

## 実C:の名前・サイズ統計(2026-06-11、`fmf stats C: --name-stats`、1,268,450件)

レイアウト判断と合成ベンチ較正の一次データ(再計測はこのコマンドで):

- fold同一(lower==orig)= 73.2% / ユニーク名 53.2% / folded後ユニーク 53.0%
- 名前長(WTF-8バイト): 平均 29.7 / p50 18 / p90 90 / p99 110 / max 171
- 4GiB超ファイル = 10件(0.0008%)

設計判断・却下判断とその数値根拠は `docs/adr/` 参照。

## Rust crates(実在・成熟度確認済み)

- `ntfs-reader` 0.4.5(MIT/Apache-2.0、2026-03更新): 生$MFT全レコードスキャン(README記載ベンチ: Vec Cache 3.756s/HashMap 4.981s/No Cache 12.3s、環境記載なし)。FileInfoで name/path/size/created/modified。**全ハードリンク名は取れない(代表名1つ)**。
- `usn-journal-rs`(wangfu91、MIT、2026-05更新): MFT列挙+USN監視+FRNパス解決。参考実装として読む(依存はしない方針)。
- `windows-sys` 0.61: FSCTL定数・MFT_ENUM_DATA・USN_RECORD等完備。USNラッパーは自前実装(~200行)。
- `memchr`(memmem::Finder=SIMD substring)、`rayon`、`parking_lot`、`thiserror`、`tracing`、`xxhash-rust`。

## WinUI 3(Windows App SDK)

- **データ仮想化**: 件数既知のランダムアクセスは **非ジェネリック`IList` + `INotifyCollectionChanged` + `IItemsRangeInfo` + プレースホルダ**。現行WASDKでサポート明記(MS Learn 2026-03更新)。`IList<T>`のみでは動かない(#1809)。`ISupportIncrementalLoading`はクラッシュ報告(#6883)あり回避。ItemsView/ItemsRepeaterは両IF非対応。ItemsPanelをItemsStackPanel以外にすると仮想化が無効化。
  https://learn.microsoft.com/en-us/windows/apps/develop/performance/listview-and-gridview-data-optimization
- **トレイ/ホットキー**: ネイティブサポート無し。H.NotifyIcon.WinUI + 自前 RegisterHotKey + HWND_MESSAGE隠しウィンドウ(WM_HOTKEY)。
- **DPI**: WinUI 3テンプレートは既定Per-Monitor V2。
- **MSIX×requireAdministrator は筋が悪い**(allowElevation等の制約、Store審査ほぼ却下)→ unpackaged + self-contained 配布。
- **昇格プロセスの既知制約**: Explorerからの D&D 不可(UIPI)。昇格プロセスから直接ShellExecuteすると関連付けアプリも昇格起動 → `explorer.exe "<path>"` 経由で脱昇格(定石)。
- WASDK 1.6+でNative AOT対応(公式サンプルで起動約50%短縮)。ただし「即時起動」体験はトレイ常駐+ホットキーで担保するのが本筋。

## セキュリティ — v2 サービス分離(2026-06-11 調査、一次情報確認済み)

特権インデクサ→非特権UIの構成は、ACL上見えないはずのファイル名・パスを露出させる情報漏洩を内包する。v2の脅威モデルと防御は `docs/SECURITY.md`、判断記録は ADR-0016/0017。以下はその裏取り:

- **PIPE_REJECT_REMOTE_CLIENTS**(CreateNamedPipeW dwPipeMode): 「Connections from remote clients are automatically rejected」と公式明記。リモート拒否の直接機構。
  https://learn.microsoft.com/en-us/windows/win32/api/namedpipeapi/nf-namedpipeapi-createnamedpipew
- **FILE_FLAG_FIRST_PIPE_INSTANCE**: 2個目のインスタンス作成が ERROR_ACCESS_DENIED で失敗(公式明記)。pipe名スクワッティング対策。出典同上。
- **GetNamedPipeServerProcessId**: クライアントからサーバプロセスPIDを取得可能(偽サーバ検出: PID→トークンがSYSTEMか検証)。
  https://learn.microsoft.com/en-us/windows/win32/api/winbase/nf-winbase-getnamedpipeserverprocessid
- **匿名アクセス(注意)**: NullSessionPipes による匿名制限の既定は**マシン種別/ポリシー依存**(DC/スタンドアロンは有効、メンバー/クライアントは Not defined)。匿名遮断は明示DACL(匿名ACEなし=既定拒否)を一次防御にすること。
  https://learn.microsoft.com/en-us/previous-versions/windows/it-pro/windows-10/security/threat-protection/security-policy-settings/network-access-restrict-anonymous-access-to-named-pipes-and-shares
- **UACフィルタ済みトークンの deny-only Administrators**: 非昇格プロセスでは BUILTIN\Administrators SID が SE_GROUP_USE_FOR_DENY_ONLY となり、**許可ACEには使われない**(deny ACE照合のみ)。「Administratorsに許可」のpipe DACLでは非昇格UIは接続できない → 利用者の個別SID名指しが必須。
  https://learn.microsoft.com/en-us/windows/win32/secauthz/sid-attributes-in-an-access-token
- **ImpersonateNamedPipeClient**: サーバがクライアントのトークンを取得・検査できる(接続時SID照合 = DACL設定ミスへの多重防御)。
  https://learn.microsoft.com/en-us/windows/win32/ipc/impersonating-a-named-pipe-client
- **SERVICE_CONFIG_REQUIRED_PRIVILEGES_INFO**(ChangeServiceConfig2): 必要特権を宣言すると SCM が起動時に宣言外特権をプロセストークンから除去(SeChangeNotifyPrivilege は常に残る。同一プロセス共有サービスは和集合)。LocalSystem の武装解除に使う。
  https://learn.microsoft.com/en-us/windows/win32/api/winsvc/ns-winsvc-service_required_privileges_infow
- **SERVICE_CONTROL_PRESHUTDOWN(注意)**: 猶予の既定は **Windows 10 1703 以降 10 秒**(それ以前は3分)。大きいスナップショット保存には `SERVICE_PRESHUTDOWN_INFO`(dwPreshutdownTimeout)で明示延長が必要。
  https://learn.microsoft.com/en-us/windows/win32/api/winsvc/ns-winsvc-service_preshutdown_info
- **windows-service crate**(Mullvad、v0.8.1 2026-05、MIT/Apache-2.0): define_windows_service! と service_control_handler::register を提供。PRESHUTDOWN ハンドラ登録可。
  https://github.com/mullvad/windows-service-rs
- **SeBackupPrivilege と生ボリューム読み**: 文書化されているのは「通常ファイルのACLバイパスでの内容取得」まで。\\.\C: の生ボリュームハンドルが SeBackupPrivilege 単独で開ける保証は**文書上存在しない**(調査範囲: Managing Privileges in a File System ほか)。ボリュームハンドルは管理者必須(上記「権限」項)→ ADR-0017 が専用低権限アカウント案を却下した根拠。
  https://learn.microsoft.com/en-us/windows-hardware/drivers/ifs/privileges

## 正規表現エンジン(rust `regex` crate、2026-06-15 調査・第一級化の前提=ADR-0023)

- **線形時間保証・ReDoS 不在**: `regex` crate は有限オートマトン(lazy DFA / Pike VM)で実装され、**バックトラッキングを行わない**。マッチは「入力長 × パターン長」に**線形**で、正規表現サービス系を悩ます catastrophic backtracking(ReDoS の実行時指数爆発)は**構造的に発生しない**と公式に明記。`(a+)+$` 系の悪性パターンでも実行は線形。
  https://docs.rs/regex/latest/regex/#untrusted-input
  https://docs.rs/regex/latest/regex/#performance
- **残る攻撃面=コンパイル時間/メモリ**: 信頼できないパターンを受ける場合の唯一の DoS 面は、巨大パターン(`a{1000}{1000}` のような有界繰り返し展開)が要求する**コンパイル時のプログラム/DFA サイズ**。crate は `RegexBuilder::size_limit`(コンパイル済みプログラムのバイト上限・**既定 10 MiB**)と `dfa_size_limit`(lazy DFA キャッシュのバイト上限・**既定 2 MiB**)を提供し、超過時は `build()` が `Error`(`CompiledTooBig` 相当)を返す。`nest_limit`(既定 250)はパース木の深さ上限。信頼できないパターンには**両 size limit を絞れ**と公式が推奨。
  https://docs.rs/regex/latest/regex/struct.RegexBuilder.html#method.size_limit
  https://docs.rs/regex/latest/regex/struct.RegexBuilder.html#method.dfa_size_limit
- find-my-files の採用値は 各 1 MiB(名前長 p99 ≈110B に対し過剰に余裕、正当パターンは到達せず、悪性パターンは `FMF_E_QUERY_SYNTAX` で丁寧に拒否)。決定と再検討トリガは ADR-0023。
