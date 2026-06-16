# find-my-files — C# API リファレンス

WinUI 3 アプリ(`app/FindMyFiles`)の公開 API リファレンス。`FindMyFiles.dll` の
XML ドキュメントコメントから自動生成しています。

アプリはエンジンに [`IEngineClient`](xref:FindMyFiles.Engine.IEngineClient) 境界経由で
のみ触れます(実装は Pipe / FFI / Fake の3種)。ViewModel 層・Services 層・エンジン
DTO もここに含まれます。

左のナビゲーション、または [`FindMyFiles.Engine` 名前空間](xref:FindMyFiles.Engine)
から辿れます。
