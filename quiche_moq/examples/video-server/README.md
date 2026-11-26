# video-server

MoQ video publisher on `quiche_moq`: runs `ffmpeg` (must be on `PATH`) to
encode a 1080p30 H.264 test pattern as fragmented MP4 and sends the MP4 boxes
on track `mp4` to every subscriber. Listens on UDP 8080.
