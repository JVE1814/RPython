# Root causes (test_free_threading.test_tokenize):
#  * tokenize did not expose the `_tokenize` module (imported lazily inside a function);
#  * TokenizerIter had no lock (shared between threads);
#  * without extra tokens, ENDMARKER sits on the last line at column -1 with that
#    line's text (it was reported like the extra-tokens form: (3, 0), line '').
import io
import tokenize

it = tokenize._tokenize.TokenizerIter(io.StringIO("for _ in a:\n  pass").readline,
                                      extra_tokens=False)
toks = [tokenize.TokenInfo._make(t) for t in it]
assert toks[-1] == tokenize.TokenInfo(0, "", (2, -1), (2, -1), "  pass"), toks[-1]
print("OK")
