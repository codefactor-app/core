use analyzer_core::{ir::AccessorKind, LanguageStrategy};
use std::path::Path;

#[test]
fn accessors_use_structure_in_every_language_and_do_not_trust_names() {
    let cases: Vec<(Box<dyn LanguageStrategy>, &str, &str)> = vec![
        (Box::new(analyzer_python::Python), "shapes.py", "class Box:\n    def read(self):\n        return self.value\n    def write(self, value):\n        self.value = value\n    def getComputed(self):\n        return self.value * 2\n    def setComputed(self, value):\n        self.value = value + 1\n    def ignored(self):\n        self.value\n    def nested(self):\n        def inner():\n            return self.value\n        return inner()\n"),
        (Box::new(analyzer_javascript::JavaScript), "shapes.js", "class Box { read(){return this.value;} write(value){this.value=value;} getComputed(){return this.value*2;} setComputed(value){this.value=value+1;} ignored(){this.value;} nested(){function inner(){return this.value;}return inner();} }"),
        (Box::new(analyzer_typescript::TypeScript), "shapes.ts", "class Box { value:number=0; read():number{return this.value;} write(value:number){this.value=value;} getComputed():number{return this.value*2;} setComputed(value:number){this.value=value+1;} ignored(){this.value;} nested(){function inner(){return this.value;}return inner();} }"),
        (Box::new(analyzer_php::Php), "shapes.php", "<?php class Box { private int $value; function read(){return $this->value;} function write($value){$this->value=$value;} function getComputed(){return $this->value*2;} function setComputed($value){$this->value=$value+1;} function ignored(){$this->value;} function nested(){function inner(){return $this->value;} return inner();} }"),
        (Box::new(analyzer_rust::Rust), "shapes.rs", "struct Box { value:i32 } impl Box { fn read(&self)->i32 {self.value} fn write(&mut self,value:i32){self.value=value;} fn getComputed(&self)->i32 {self.value*2} fn setComputed(&mut self,value:i32){self.value=value+1;} fn ignored(&self){self.value;} fn nested(&self){fn inner()->i32 {self.value} inner();} }"),
    ];
    for (strategy, path, source) in cases {
        let model = strategy.analyze(source, Path::new(path));
        assert!(
            model.parse_errors.is_empty(),
            "{path}: {:?}",
            model.parse_errors
        );
        let find = |name: &str| model.functions.iter().find(|f| f.name == name).unwrap();
        assert_eq!(
            find("read").accessor,
            Some(AccessorKind::Getter),
            "{path}: {:?}",
            find("read")
        );
        assert_eq!(
            find("write").accessor,
            Some(AccessorKind::Setter),
            "{path}: {:?}",
            find("write")
        );
        assert!(find("getComputed").accessor.is_none(), "{path}");
        assert!(find("setComputed").accessor.is_none(), "{path}");
        assert!(
            find("ignored").accessor.is_none(),
            "{path}: discarded field read"
        );
        assert!(
            find("inner").accessor.is_none(),
            "{path}: nested helper is not a method"
        );
        assert!(find("read").parent_type.is_some(), "{path}");
    }
}
