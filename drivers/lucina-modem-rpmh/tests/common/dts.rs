//! A reader for the few lines of `dtc` output the tests need: nested nodes, `name = <cells>, "strings";` properties.

#[derive(Debug, Default)]
pub struct Node {
    pub name: String,
    pub props: Vec<(String, Vec<u8>)>,
    pub kids: Vec<Node>,
}

impl Node {
    pub fn prop(&self, name: &str) -> Option<&[u8]> {
        self.props
            .iter()
            .find(|(n, _)| n == name)
            .map(|(_, v)| v.as_slice())
    }
    pub fn has(&self, name: &str) -> bool {
        self.prop(name).is_some()
    }
    /// A property as big-endian 32-bit cells.
    pub fn cells(&self, name: &str) -> Vec<u32> {
        let v = self
            .prop(name)
            .unwrap_or_else(|| panic!("{}: no property {name}", self.name));
        assert_eq!(v.len() % 4, 0, "{}: {name} is not whole cells", self.name);
        v.chunks(4)
            .map(|c| u32::from_be_bytes([c[0], c[1], c[2], c[3]]))
            .collect()
    }
    pub fn cell(&self, name: &str) -> u32 {
        let c = self.cells(name);
        assert_eq!(c.len(), 1, "{}: {name} is not one cell", self.name);
        c[0]
    }
    /// A string-list property.
    pub fn strings(&self, name: &str) -> Vec<String> {
        let v = self
            .prop(name)
            .unwrap_or_else(|| panic!("{}: no property {name}", self.name));
        assert_eq!(
            v.last(),
            Some(&0),
            "{}: {name} is not a string list",
            self.name
        );
        v[..v.len() - 1]
            .split(|&b| b == 0)
            .map(|s| String::from_utf8(s.to_vec()).unwrap())
            .collect()
    }
    pub fn kid(&self, name: &str) -> &Node {
        self.kids
            .iter()
            .find(|k| k.name == name)
            .unwrap_or_else(|| panic!("{}: no child {name}", self.name))
    }
    /// The chain of nodes from `self` down to the one with `phandle`, itself included.
    pub fn find_phandle(&self, ph: u32) -> Option<Vec<&Node>> {
        if self.prop("phandle").is_some() && self.cell("phandle") == ph {
            return Some(vec![self]);
        }
        for k in &self.kids {
            if let Some(mut chain) = k.find_phandle(ph) {
                chain.insert(0, self);
                return Some(chain);
            }
        }
        None
    }
}

struct P<'a> {
    s: &'a [u8],
    i: usize,
}

impl P<'_> {
    fn ws(&mut self) {
        loop {
            while self.i < self.s.len() && self.s[self.i].is_ascii_whitespace() {
                self.i += 1;
            }
            if self.s[self.i..].starts_with(b"//") {
                while self.i < self.s.len() && self.s[self.i] != b'\n' {
                    self.i += 1;
                }
            } else {
                return;
            }
        }
    }
    fn peek(&self) -> Option<u8> {
        self.s.get(self.i).copied()
    }
    fn name(&mut self) -> String {
        let st = self.i;
        while self.i < self.s.len()
            && (self.s[self.i].is_ascii_alphanumeric() || b",._+*#?@-".contains(&self.s[self.i]))
        {
            self.i += 1;
        }
        assert!(self.i > st, "expected a name at byte {}", self.i);
        String::from_utf8(self.s[st..self.i].to_vec()).unwrap()
    }
    fn expect(&mut self, c: u8) {
        self.ws();
        assert_eq!(
            self.peek(),
            Some(c),
            "expected {:?} at byte {}",
            c as char,
            self.i
        );
        self.i += 1;
    }
    fn string(&mut self) -> Vec<u8> {
        self.expect(b'"');
        let mut out = Vec::new();
        loop {
            let c = self.s[self.i];
            self.i += 1;
            match c {
                b'"' => break,
                b'\\' => {
                    let e = self.s[self.i];
                    self.i += 1;
                    match e {
                        b'n' => out.push(b'\n'),
                        b't' => out.push(b'\t'),
                        b'f' => out.push(0x0c),
                        b'a' => out.push(7),
                        b'b' => out.push(8),
                        b'r' => out.push(b'\r'),
                        b'v' => out.push(0x0b),
                        b'x' => {
                            let h = std::str::from_utf8(&self.s[self.i..self.i + 2]).unwrap();
                            out.push(u8::from_str_radix(h, 16).unwrap());
                            self.i += 2;
                        }
                        b'0'..=b'7' => {
                            let mut v = u32::from(e - b'0');
                            for _ in 0..2 {
                                if matches!(self.s[self.i], b'0'..=b'7') {
                                    v = v * 8 + u32::from(self.s[self.i] - b'0');
                                    self.i += 1;
                                }
                            }
                            out.push(v as u8);
                        }
                        other => out.push(other),
                    }
                }
                c => out.push(c),
            }
        }
        out.push(0);
        out
    }
    fn value(&mut self) -> Vec<u8> {
        self.ws();
        match self.peek() {
            Some(b'"') => self.string(),
            Some(b'<') => {
                self.i += 1;
                let mut out = Vec::new();
                loop {
                    self.ws();
                    if self.peek() == Some(b'>') {
                        self.i += 1;
                        break;
                    }
                    let st = self.i;
                    while self.s[self.i].is_ascii_alphanumeric() {
                        self.i += 1;
                    }
                    let t = std::str::from_utf8(&self.s[st..self.i]).unwrap();
                    let v = match t.strip_prefix("0x") {
                        Some(h) => u32::from_str_radix(h, 16).unwrap(),
                        None => t.parse().unwrap(),
                    };
                    out.extend_from_slice(&v.to_be_bytes());
                }
                out
            }
            other => panic!("unexpected value start {other:?} at byte {}", self.i),
        }
    }
    fn node(&mut self, name: String) -> Node {
        let mut n = Node {
            name,
            ..Default::default()
        };
        loop {
            self.ws();
            if self.peek() == Some(b'}') {
                self.i += 1;
                self.expect(b';');
                return n;
            }
            let nm = self.name();
            self.ws();
            match self.peek() {
                Some(b'{') => {
                    self.i += 1;
                    let kid = self.node(nm);
                    n.kids.push(kid);
                }
                Some(b'=') => {
                    self.i += 1;
                    let mut v = self.value();
                    loop {
                        self.ws();
                        if self.peek() == Some(b',') {
                            self.i += 1;
                            v.extend(self.value());
                        } else {
                            break;
                        }
                    }
                    self.expect(b';');
                    n.props.push((nm, v));
                }
                Some(b';') => {
                    self.i += 1;
                    n.props.push((nm, Vec::new()));
                }
                other => panic!("unexpected {other:?} after {nm} at byte {}", self.i),
            }
        }
    }
}

/// Parse the excerpt: a list of top-level nodes, returned as children of an unnamed root.
pub fn parse(text: &str) -> Node {
    let mut p = P {
        s: text.as_bytes(),
        i: 0,
    };
    let mut root = Node::default();
    loop {
        p.ws();
        if p.i >= p.s.len() {
            return root;
        }
        let nm = p.name();
        p.expect(b'{');
        let kid = p.node(nm);
        root.kids.push(kid);
    }
}
