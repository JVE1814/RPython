//! Minimal backtracking regex (enough for the patterns used in the CPython tests'
//! assertRaisesRegex calls): literals, escapes, '.', classes, groups with '|', '*', '+', '?',
//! '{m,n}', '^', '$'. `search` semantics.

#[derive(Debug, Clone)]
enum Node {
    Char(char),
    Any,
    Class(Vec<(char, char)>, bool),
    Group(Vec<Vec<Node>>),
    Repeat(Box<Node>, usize, usize),
    Start,
    End,
}

struct P<'a> {
    s: &'a [char],
    i: usize,
}

impl<'a> P<'a> {
    fn alt(&mut self) -> Vec<Vec<Node>> {
        let mut alts = vec![self.seq()];
        while self.i < self.s.len() && self.s[self.i] == '|' {
            self.i += 1;
            alts.push(self.seq());
        }
        alts
    }
    fn seq(&mut self) -> Vec<Node> {
        let mut v = Vec::new();
        while self.i < self.s.len() && self.s[self.i] != '|' && self.s[self.i] != ')' {
            let atom = self.atom();
            let atom = self.quant(atom);
            v.push(atom);
        }
        v
    }
    fn escape_class(c: char) -> Option<(Vec<(char, char)>, bool)> {
        Some(match c {
            'd' => (vec![('0', '9')], false),
            'D' => (vec![('0', '9')], true),
            'w' => (vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')], false),
            'W' => (vec![('a', 'z'), ('A', 'Z'), ('0', '9'), ('_', '_')], true),
            's' => (vec![(' ', ' '), ('\t', '\r')], false),
            'S' => (vec![(' ', ' '), ('\t', '\r')], true),
            _ => return None,
        })
    }
    fn atom(&mut self) -> Node {
        let c = self.s[self.i];
        self.i += 1;
        match c {
            '.' => Node::Any,
            '^' => Node::Start,
            '$' => Node::End,
            '(' => {
                if self.i + 1 < self.s.len() && self.s[self.i] == '?' && self.s[self.i + 1] == ':' {
                    self.i += 2;
                }
                let a = self.alt();
                self.i += 1; // ')'
                Node::Group(a)
            }
            '[' => {
                let mut neg = false;
                if self.s[self.i] == '^' {
                    neg = true;
                    self.i += 1;
                }
                let mut ranges = Vec::new();
                let mut first = true;
                while self.s[self.i] != ']' || first {
                    first = false;
                    let mut a = self.s[self.i];
                    self.i += 1;
                    if a == '\\' {
                        let e = self.s[self.i];
                        self.i += 1;
                        if let Some((r, _)) = Self::escape_class(e) {
                            ranges.extend(r);
                            continue;
                        }
                        a = unesc(e);
                    }
                    if self.s[self.i] == '-' && self.s[self.i + 1] != ']' {
                        self.i += 1;
                        let mut b = self.s[self.i];
                        self.i += 1;
                        if b == '\\' {
                            b = unesc(self.s[self.i]);
                            self.i += 1;
                        }
                        ranges.push((a, b));
                    } else {
                        ranges.push((a, a));
                    }
                }
                self.i += 1;
                Node::Class(ranges, neg)
            }
            '\\' => {
                let e = self.s[self.i];
                self.i += 1;
                if let Some((r, n)) = Self::escape_class(e) {
                    return Node::Class(r, n);
                }
                Node::Char(unesc(e))
            }
            c => Node::Char(c),
        }
    }
    fn quant(&mut self, a: Node) -> Node {
        if self.i >= self.s.len() {
            return a;
        }
        let (lo, hi) = match self.s[self.i] {
            '*' => (0, usize::MAX),
            '+' => (1, usize::MAX),
            '?' => (0, 1),
            '{' if {
                let mut j = self.i + 1;
                while j < self.s.len() && (self.s[j].is_ascii_digit() || self.s[j] == ',') {
                    j += 1;
                }
                j < self.s.len() && self.s[j] == '}' && j > self.i + 1
            } => {
                let mut j = self.i + 1;
                let mut num = String::new();
                while self.s[j] != '}' {
                    num.push(self.s[j]);
                    j += 1;
                }
                let (lo, hi) = match num.split_once(',') {
                    Some((a, b)) => (a.parse().unwrap_or(0), if b.is_empty() { usize::MAX } else { b.parse().unwrap() }),
                    None => {
                        let n = num.parse().unwrap();
                        (n, n)
                    }
                };
                self.i = j;
                (lo, hi)
            }
            _ => return a,
        };
        self.i += 1;
        if self.i < self.s.len() && self.s[self.i] == '?' {
            self.i += 1;
        }
        Node::Repeat(Box::new(a), lo, hi)
    }
}

fn unesc(c: char) -> char {
    match c {
        'n' => '\n',
        't' => '\t',
        'r' => '\r',
        c => c,
    }
}

fn m_seq(nodes: &[Node], s: &[char], i: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
    if nodes.is_empty() {
        return k(i);
    }
    let (first, rest) = (&nodes[0], &nodes[1..]);
    m_node(first, s, i, &mut |j| m_seq(rest, s, j, k))
}

fn m_node(n: &Node, s: &[char], i: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
    match n {
        Node::Char(c) => i < s.len() && s[i] == *c && k(i + 1),
        Node::Any => i < s.len() && s[i] != '\n' && k(i + 1),
        Node::Class(r, neg) => {
            if i >= s.len() {
                return false;
            }
            let inr = r.iter().any(|&(a, b)| s[i] >= a && s[i] <= b);
            inr != *neg && k(i + 1)
        }
        Node::Start => i == 0 && k(i),
        Node::End => (i == s.len() || (i + 1 == s.len() && s[i] == '\n')) && k(i),
        Node::Group(alts) => alts.iter().any(|a| m_seq(a, s, i, k)),
        Node::Repeat(a, lo, hi) => m_rep(a, *lo, *hi, s, i, 0, k),
    }
}

fn m_rep(a: &Node, lo: usize, hi: usize, s: &[char], i: usize, count: usize, k: &mut dyn FnMut(usize) -> bool) -> bool {
    // greedy
    if count < hi {
        let ok = m_node(a, s, i, &mut |j| j != i && m_rep(a, lo, hi, s, j, count + 1, k));
        if ok {
            return true;
        }
    }
    count >= lo && k(i)
}

pub fn search(pattern: &str, text: &str) -> bool {
    let pc: Vec<char> = pattern.chars().collect();
    let mut p = P { s: &pc, i: 0 };
    let alts = p.alt();
    let node = Node::Group(alts);
    let tc: Vec<char> = text.chars().collect();
    for start in 0..=tc.len() {
        if m_node(&node, &tc, start, &mut |_| true) {
            return true;
        }
    }
    false
}
