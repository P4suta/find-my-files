using Xunit;

// Tests touch process-wide statics (Notifier's post funnel, FileLog's shared
// app.log, the AppSettings paths). Run them serially so those globals cannot
// race across collections — the suite is small, so determinism is worth more
// than the parallelism here.
[assembly: CollectionBehavior(DisableTestParallelization = true)]
