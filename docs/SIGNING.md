# コード署名 — 配布物の Authenticode 署名

配布 zip 内の自前バイナリ(`FindMyFiles.exe` ほか)を **SSL.com eSigner**(クラウド HSM 署名)で
Authenticode 署名するための手順書。判断の経緯と却下案は [ADR-0020](adr/0020-code-signing-provider.md)。

## 現状

`.github/workflows/release.yml` に署名ステップは**配線済みだが休眠**している。署名は **非ブロッキング**で、
リポジトリ Secrets(`ES_USERNAME` / `CREDENTIAL_ID`)が揃うまではタグを切っても **未署名のまま `::warning::` を出して完走**する。
**証明書を取得し、下記4つの Secrets を登録した次のタグから自動で署名が有効化される。** それ以外に CI を触る必要はない。

署名対象は**自前の PE のみ** — `FindMyFiles.exe`(ユーザーが起動する本体 = SmartScreen 判定の主対象)、`fmf.exe`、
`fmf-service.exe`、`fmf_engine.dll`。同梱の .NET / WindowsAppSDK ランタイム DLL は既に Microsoft 署名済みのため再署名しない
(署名クォータの浪費と他者著作物への署名を避ける)。

## 前提知識(なぜこの構成か)

- **日本在住の個人**は Azure Artifact Signing(旧 Trusted Signing)の個人枠が**対象外**(米/加/EU/英のみ)。
- **EV 署名はもう SmartScreen の即時信頼を与えない**(Microsoft が2024年3月に変更)。本アプリはカーネルドライバを積まない
  ため、EV を取っても実利はほぼ無い。よって個人名義の **IV(Individual Validation)** で十分。
- SmartScreen は評判ベース。署名しても**初回は警告が出ることがあり**、ダウンロード実績が積み上がると消える。署名の即効効果は
  「不明な発行元」が消え、プロパティに**自分の名前**が出ること。

## 有効化手順(証明書を取りたくなったら)

### A. 証明書の取得(SSL.com)

1. [SSL.com](https://www.ssl.com/) でアカウントを作成。
2. **Code Signing** の証明書を購入。**eSigner(クラウド署名)対応の Individual Validation(IV)** を選ぶ
   (USB トークン版ではなくクラウド版)。目安 年 $130〜250。
   - EV の肩書きが欲しい場合のみ **Sole Proprietor EV**(法人登記不要)を選んでもよい。**本リポジトリの CI は変更不要**
     (同じ Action・同じ4 Secrets)。ただし SmartScreen 挙動は IV と同等。

### B. 本人確認(IV validation)

3. 政府発行 ID + 本人確認(書類/ビデオ)。**法人登記は不要**。日本の個人/個人事業主の取得実績あり。

### C. eSigner を自動署名用に設定

4. SSL.com ダッシュボードで:
   - 署名証明書の **Credential ID** を控える。
   - **自動署名用の TOTP(2FA)シークレット**(Base32 文字列)を発行して控える。
   - アカウントの **ユーザー名 / パスワード**。

### D. GitHub Secrets を4つ登録

5. リポジトリ → Settings → Secrets and variables → Actions → New repository secret で:

   | Secret 名 | 値 |
   |---|---|
   | `ES_USERNAME` | SSL.com ユーザー名 |
   | `ES_PASSWORD` | SSL.com パスワード |
   | `CREDENTIAL_ID` | 署名証明書の Credential ID |
   | `ES_TOTP_SECRET` | eSigner 自動署名用 TOTP シークレット(Base32) |

   → 次に `vX.Y.Z` タグを切る(または `release` を `workflow_dispatch`)と、`HAVE_SIGNING` が `true` になり署名が走る。

### E. 検証

6. **空打ち(証明書取得前でも今すぐ可)**: Secrets 未設定のまま Actions → release を `workflow_dispatch` 実行 →
   署名ステップが skip され `::warning::` が出て、zip / checksum / Release 作成まで**失敗せず完走**することを確認
   (= 休眠配管がパイプラインを壊さない)。
7. **本署名(Secrets 登録後)**: テストタグ(例 `v0.0.1-rc1`)を切って実行。「Sign staged binaries」が走り、
   「Verify signatures」が4ファイルとも `signed: ... - CN=<あなたの名前>` を出して green になることを確認。
8. **ローカル確認**: Release の zip を展開し、Windows で:
   ```powershell
   signtool verify /pa /v dist\FindMyFiles\FindMyFiles.exe   # → Successfully verified
   Get-AuthenticodeSignature dist\FindMyFiles\FindMyFiles.exe # → Status: Valid
   ```
   `FindMyFiles.exe` のプロパティ →「デジタル署名」タブに自分の名前とタイムスタンプが出る。

## 更新(失効対応)

- 公開信頼コード署名証明書の有効期間は CA/Browser Forum 規定で **最長 ~460日(約15か月)**。失効前に SSL.com で更新する。
- 更新で **Credential ID / TOTP が変わる場合のみ** 対応する Secret を更新する。

## トラブルシュート

- **Verify signatures が落ちる**: `batch_sign` が `override` ではなく別フォルダへ署名済みファイルを書いた可能性。
  Action のログで出力先を確認し、必要なら `release.yml` の署名ステップに `output_path` を指定して「Copy signed binaries back」
  のコピー元をそこに合わせる。
- **`Get-AuthenticodeSignature` が `UnknownError`**: 公開信頼チェーン未解決。`signtool verify /pa` で詳細を確認。
- **初回起動でまだ SmartScreen 警告が出る**: 仕様(評判が浅いため)。ダウンロードが積み上がると消える。EV でも同様。

## 関連

- [ADR-0020 — コード署名プロバイダ選定](adr/0020-code-signing-provider.md)
- [SECURITY.md](SECURITY.md)
- 配線本体: `.github/workflows/release.yml`
