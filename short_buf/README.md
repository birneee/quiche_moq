# short_buf

A short, fixed-capacity buffer queue (`ShortBuf<const N: usize>`) with no
dynamic allocation, for streams between a reader and a writer where most
data passes straight through and only a small amount ever needs buffering
(e.g. lookahead or partial-message parsing). When the buffer is empty, reads
pass through without an extra copy.
