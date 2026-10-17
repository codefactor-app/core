<div>mixed HTML</div>
<?php
namespace App\Example;
use App\Database\Store as Database;

interface Repository { public function query(string $sql): string; }
trait Logged { public function log(string $message) { return $message; } }

class Service implements Repository {
    private Database $store;
    public function __construct(Database $store) { $this->store = $store; }
    public function query(string $sql): string { return $sql; }
    public function run(array $items) {
        foreach ($items as $item) {
            if ($item) { $this->store->query($item); }
            else { shell_exec($item); }
        }
    }
}
