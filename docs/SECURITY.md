# セキュリティ — 脅威モデルと防御(v2 サービス分離)

現在の構成: 特権サービス `fmf-service`(LocalSystem・特権最小化)が NTFS の $MFT/USN を読み、
非特権UIが named pipe で接続する。判断の経緯と却下案は
[ADR-0016](adr/0016-service-split-named-pipe.md) / [ADR-0017](adr/0017-service-security-model.md)、
API仕様の裏取りは [RESEARCH.md](RESEARCH.md)。

## 脅威と防御

| # | 脅威 | 防御 |
|---|---|---|
| 1 | ACL迂回の名前漏洩 — 特権インデクサが、利用者のACLでは見えないファイル名を**別ユーザー**へ露出する(Everything の ETP サーバで事件化した形) | pipe DACL を SYSTEM+利用者SIDに限定(install時捕捉のSID **+ 非昇格UIが `--owner-sid` で転送する日常ユーザーSID**。後者は `validate_user_sid` で実在ユーザー型のみ採用 — OTS昇格でも日常ユーザーを締め出さず、かつ任意SIDの混入を防ぐ)。Authenticated Users / Everyone のACEなし(既定拒否)+接続時トークン照合 |
| 2 | リモート接続 | `PIPE_REJECT_REMOTE_CLIENTS`(+サーバ機能はやらないことリストで恒久非実装) |
| 3 | 匿名接続 | 明示DACLに匿名ACEなし=既定拒否(NullSessionPipes の既定はポリシー依存のため当てにしない) |
| 4 | pipe名スクワッティング / 偽サーバ | サーバ: **初回インスタンスのみ** `FILE_FLAG_FIRST_PIPE_INSTANCE`(2本目以降はフラグ無し — 初回を保持し続ける限り名前の先取りは不能)。クライアント: 既定pipe名では `GetNamedPipeServerProcessId` → **SCM登録の fmf-engine サービスPIDと照合**(`QueryServiceStatusEx`。非昇格UIで動く — SYSTEMプロセスのトークンは非昇格では開けず[ACCESS_DENIED]、session 0 プロセスの identity も取得不可。squatter は SCM登録[要admin]ができずPIDが一致しない) |
| 5 | 悪意あるクライアント入力(不正フレーム・巨大len・未知opcode) | 長さ上限16MiB・検証失敗は接続切断+`pipe_malformed_frames` カウンタ。dispatcher 全体が catch_unwind 防火壁(panic は FMF_E_PANIC 応答、サービスは生存) |
| 6 | ローカルDoS(接続洪水・ハンドル枯渇・flush連打) | pipe インスタンス上限 8(超過は接続拒否+`pipe_connections_rejected`)。結果ハンドル上限64/接続(LRU evict→STALE)。Flush は pipe に公開しない(サービス内部の定期+停止時のみ)。イベントは有界キュー+ドロップで USN スレッドを保護。なお到達できるのは認可済み同一ユーザーのみ(#1) |
| 7 | データファイル自体の漏洩(.fmfidx は全ボリュームのファイル名を含む) | install 時に `%ProgramData%\find-my-files` へ保護DACL(SYSTEM+Administrators。logs サブディレクトリのみ利用者read)。uninstall は既定でデータ保持(残置物を案内表示)、`--purge-data` で削除 |
| 8 | 残存リスク(受容) | 認可済みユーザーは自分のACLで見えないファイルの「名前・パス」も検索できる(ファイル名のみ索引の構造的性質。内容・ACL実体は読めない)。単一ユーザー機を主対象とし、マルチユーザー認可は ADR-0017 の再検討トリガ |

## 配布物の完全性(コード署名)

配布バイナリの Authenticode 署名は SSL.com eSigner(個人IV)で行う。配線は `release.yml` に休眠状態で組み込み済みで、
証明書取得後に GitHub Secrets を入れると有効化される。取得・有効化手順は [SIGNING.md](SIGNING.md)、選定の根拠は
[ADR-0020](adr/0020-code-signing-provider.md)。署名はタグ駆動 `release.yml` 限定(`ci.yml` の開発成果物は署名しない)。

## 手動検証チェックリスト(リリース前に1回実施し、結果と日付をここに記録)

自動化できない項目(別ユーザートークン・別マシンが必要)。SDDL構築関数の構造は単体テストでピン済み。

- [ ] 別ユーザー(非認可SID)からの pipe 接続が拒否される
- [ ] リモートからの `\\<host>\pipe\fmf-engine-v1` 接続が拒否される
- [ ] 非昇格プロセスから `%ProgramData%\find-my-files\index\*.fmfidx` が読めない
- [ ] 非昇格プロセスから `%ProgramData%\find-my-files\logs\engine.log` は読める(F12診断動線)
- [ ] OTS昇格(別の管理者アカウントで昇格)後も、日常ユーザーが非昇格で pipe 接続できる(`--owner-sid` 伝搬)
- [ ] 稼働中サービスへ「登録し直す」→ 再起動で `authorized_sids` が反映され、それまで拒否されていたユーザーが接続できる(`pipe client token rejected` が止む)
- [ ] `fmf-service uninstall` 後の残置物が案内どおり / `--purge-data` で消える

実施記録: (未実施)
